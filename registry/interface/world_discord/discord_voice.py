import math
import os
import struct
import subprocess
import tempfile
import threading
import time

SAMPLE_RATE = 48000
CHANNELS = 2
SAMPLE_WIDTH = 2
FRAME_MS = 20
SAMPLES_PER_FRAME = 960
FRAME_SIZE = 3840
SILENCE = b"\x00" * FRAME_SIZE


def _numpy():
    import numpy
    return numpy


class MixerChild:
    __slots__ = ("pcm", "position", "loop", "gain", "speech", "fade_frames", "fade_done", "finished")

    def __init__(self, pcm, loop=False, gain=1.0, speech=False, fade_ms=0):
        remainder = len(pcm) % FRAME_SIZE
        self.pcm = pcm + (b"\x00" * (FRAME_SIZE - remainder) if remainder else b"")
        self.position = 0
        self.loop = loop
        self.gain = float(gain)
        self.speech = speech
        self.fade_frames = max(0, int(fade_ms) // FRAME_MS)
        self.fade_done = 0
        self.finished = False

    def read(self):
        if self.finished:
            return None
        if self.position >= len(self.pcm):
            if self.loop and self.pcm:
                self.position = 0
            else:
                self.finished = True
                return None
        chunk = self.pcm[self.position:self.position + FRAME_SIZE]
        self.position += FRAME_SIZE
        if len(chunk) < FRAME_SIZE:
            chunk += b"\x00" * (FRAME_SIZE - len(chunk))
        np = _numpy()
        samples = np.frombuffer(chunk, dtype=np.int16).astype(np.float32)
        gain = self.gain
        if self.fade_frames and self.fade_done < self.fade_frames:
            self.fade_done += 1
            gain *= self.fade_done / self.fade_frames
        return samples if gain == 1.0 else samples * gain


class VoiceMixer:
    def __init__(self, ambient_gain=0.18, duck_gain=0.06, speech_gain=1.0, duck_release_ms=400):
        self.lock = threading.Lock()
        self.ambient = None
        self.speech = []
        self.ambient_gain = float(ambient_gain)
        self.duck_gain = float(duck_gain)
        self.speech_gain = float(speech_gain)
        self.duck_release_frames = max(1, int(duck_release_ms) // FRAME_MS)
        self.duck_release_left = 0
        self.closed = False

    def is_opus(self):
        return False

    def set_ambient(self, pcm):
        with self.lock:
            self.ambient = None if not pcm else MixerChild(pcm, loop=True, gain=self.ambient_gain, fade_ms=200)

    def play_speech(self, pcm, gain=None):
        if not pcm:
            return
        with self.lock:
            self.speech.append(MixerChild(pcm, gain=self.speech_gain if gain is None else gain, speech=True, fade_ms=40))
            self.duck_release_left = 0
            if self.ambient is not None:
                self.ambient.gain = self.duck_gain

    @property
    def speech_active(self):
        with self.lock:
            return bool(self.speech)

    def stop_speech(self):
        with self.lock:
            self.speech.clear()
            self.duck_release_left = self.duck_release_frames

    def read(self):
        np = _numpy()
        with self.lock:
            if self.closed:
                return b""
            active = []
            if self.ambient is not None:
                frame = self.ambient.read()
                if frame is not None:
                    active.append(frame)
            remaining = []
            for child in self.speech:
                frame = child.read()
                if frame is not None:
                    active.append(frame)
                if not child.finished:
                    remaining.append(child)
            had_speech = bool(self.speech)
            self.speech = remaining
            if had_speech and not remaining:
                self.duck_release_left = self.duck_release_frames
            if self.ambient is not None:
                if remaining:
                    self.ambient.gain = self.duck_gain
                elif self.duck_release_left:
                    progress = 1 - self.duck_release_left / self.duck_release_frames
                    self.ambient.gain = self.duck_gain + (self.ambient_gain - self.duck_gain) * progress
                    self.duck_release_left -= 1
                else:
                    self.ambient.gain = self.ambient_gain
            if not active:
                return SILENCE
            mixed = np.zeros(SAMPLES_PER_FRAME * CHANNELS, dtype=np.float32)
            for frame in active:
                mixed += frame
            return np.clip(mixed, -32768, 32767).astype(np.int16).tobytes()

    def cleanup(self):
        with self.lock:
            self.closed = True
            self.speech.clear()
            self.ambient = None


def decode_to_pcm(path, maximum_bytes, timeout=30):
    if isinstance(maximum_bytes, bool) or not isinstance(maximum_bytes, int) or maximum_bytes < FRAME_SIZE:
        raise ValueError("maximum decoded PCM bytes must be a positive frame-sized ceiling")
    try:
        with tempfile.TemporaryFile() as output:
            process = subprocess.run(
                ["ffmpeg", "-v", "error", "-i", path, "-fs", str(maximum_bytes + 1), "-f", "s16le", "-ar", "48000", "-ac", "2", "pipe:1"],
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.DEVNULL,
                timeout=timeout,
                check=False,
            )
            size = output.tell()
            if process.returncode != 0 or size == 0 or size > maximum_bytes:
                return None
            output.seek(0)
            data = output.read(maximum_bytes + 1)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return data if len(data) == size else None


def synth_ambient_pcm(seconds=4.0):
    np = _numpy()
    frame_count = int(SAMPLE_RATE * seconds)
    t = np.arange(frame_count, dtype=np.float32) / SAMPLE_RATE
    base = 0.035 * np.sin(2 * math.pi * 110 * t)
    overtone = 0.018 * np.sin(2 * math.pi * 165 * t)
    pulse = 0.65 + 0.35 * np.sin(2 * math.pi * 0.25 * t)
    mono = np.clip((base + overtone) * pulse, -1, 1)
    stereo = np.column_stack((mono, mono)).reshape(-1)
    return (stereo * 32767).astype(np.int16).tobytes()


class VoiceReceiver:
    def __init__(self, voice_client, silence_seconds, minimum_seconds, maximum_seconds, maximum_speaker_bytes, maximum_buffered_bytes, maximum_active_speakers, user_allowed):
        self.voice_client = voice_client
        self.silence_seconds = float(silence_seconds)
        self.minimum_seconds = float(minimum_seconds)
        self.maximum_seconds = float(maximum_seconds)
        self.maximum_speaker_bytes = int(maximum_speaker_bytes)
        self.maximum_buffered_bytes = int(maximum_buffered_bytes)
        self.maximum_active_speakers = int(maximum_active_speakers)
        self.user_allowed = user_allowed
        self.running = False
        self.paused = False
        self.secret_key = None
        self.dave = None
        self.bot_ssrc = 0
        self.ssrc_users = {}
        self.buffers = {}
        self.last_packet = {}
        self.decoders = {}
        self.lock = threading.Lock()
        self.original_hook = None

    def start(self):
        connection = self.voice_client._connection
        self.secret_key = bytes(connection.secret_key)
        self.dave = getattr(connection, "dave_session", None)
        self.bot_ssrc = connection.ssrc
        self.original_hook = connection.hook
        receiver = self

        async def hook(ws, message):
            if isinstance(message, dict) and message.get("op") == 5:
                data = message.get("d") or {}
                if data.get("ssrc") and data.get("user_id"):
                    user_id = int(data["user_id"])
                    if not receiver.user_allowed(user_id):
                        if receiver.original_hook:
                            await receiver.original_hook(ws, message)
                        return
                    with receiver.lock:
                        ssrc = int(data["ssrc"])
                        if ssrc not in receiver.ssrc_users and len(receiver.ssrc_users) >= receiver.maximum_active_speakers:
                            receiver.ssrc_users.pop(next(iter(receiver.ssrc_users)))
                        receiver.ssrc_users[ssrc] = user_id
            if receiver.original_hook:
                await receiver.original_hook(ws, message)

        connection.hook = hook
        try:
            if getattr(connection, "ws", None) is not None:
                connection.ws._hook = hook
        except Exception:
            pass
        connection.add_socket_listener(self._packet)
        self.running = True

    def stop(self):
        self.running = False
        try:
            self.voice_client._connection.remove_socket_listener(self._packet)
        except Exception:
            pass
        with self.lock:
            self.buffers.clear()
            self.last_packet.clear()
            self.decoders.clear()
            self.ssrc_users.clear()

    def _packet(self, data):
        if not self.running or self.paused or len(data) < 16:
            return
        if (data[0] >> 6) != 2 or (data[1] & 0x7F) != 0x78:
            return
        first = data[0]
        _, _, _, _, ssrc = struct.unpack_from(">BBHII", data, 0)
        if ssrc == self.bot_ssrc:
            return
        csrc_count = first & 0x0F
        extension = bool(first & 0x10)
        padding = bool(first & 0x20)
        header_size = 12 + 4 * csrc_count + (4 if extension else 0)
        if len(data) < header_size + 4:
            return
        extension_bytes = 0
        if extension:
            extension_bytes = struct.unpack_from(">H", data, 12 + 4 * csrc_count + 2)[0] * 4
        header = bytes(data[:header_size])
        body = data[header_size:]
        nonce = bytearray(24)
        nonce[:4] = body[-4:]
        try:
            import nacl.secret
            decrypted = nacl.secret.Aead(self.secret_key).decrypt(bytes(body[:-4]), header, bytes(nonce))
        except Exception:
            return
        if extension_bytes and len(decrypted) > extension_bytes:
            decrypted = decrypted[extension_bytes:]
        if padding:
            if not decrypted or decrypted[-1] == 0 or decrypted[-1] > len(decrypted):
                return
            decrypted = decrypted[:-decrypted[-1]]
        with self.lock:
            user_id = self.ssrc_users.get(ssrc, 0)
            if not user_id:
                return
            if ssrc not in self.buffers and len(self.buffers) >= self.maximum_active_speakers:
                return
        if self.dave and user_id:
            try:
                import davey
                decrypted = self.dave.decrypt(user_id, davey.MediaType.audio, decrypted)
            except Exception as error:
                if "Unencrypted" not in str(error):
                    return
        try:
            import discord
            decoder = self.decoders.setdefault(ssrc, discord.opus.Decoder())
            pcm = decoder.decode(decrypted)
        except Exception:
            self.decoders.pop(ssrc, None)
            return
        with self.lock:
            buffer = self.buffers.setdefault(ssrc, bytearray())
            aggregate = sum(len(value) for value in self.buffers.values())
            remaining = min(self.maximum_speaker_bytes - len(buffer), self.maximum_buffered_bytes - aggregate)
            if remaining > 0:
                buffer.extend(pcm[:remaining])
            self.last_packet[ssrc] = time.monotonic()

    def _infer_user(self, ssrc):
        try:
            bot_id = self.voice_client.user.id
            candidates = [member.id for member in self.voice_client.channel.members if member.id != bot_id and self.user_allowed(member.id)]
            if len(candidates) == 1:
                self.ssrc_users[ssrc] = candidates[0]
                return candidates[0]
        except Exception:
            pass
        return 0

    def completed(self):
        now = time.monotonic()
        result = []
        with self.lock:
            for ssrc in list(self.buffers):
                buffer = self.buffers[ssrc]
                duration = len(buffer) / (SAMPLE_RATE * CHANNELS * SAMPLE_WIDTH)
                silence = now - self.last_packet.get(ssrc, now)
                if len(buffer) >= self.maximum_speaker_bytes or duration >= self.maximum_seconds or (silence >= self.silence_seconds and duration >= self.minimum_seconds):
                    user_id = self.ssrc_users.get(ssrc) or self._infer_user(ssrc)
                    if user_id:
                        result.append((user_id, bytes(buffer)))
                    self.buffers.pop(ssrc, None)
                    self.last_packet.pop(ssrc, None)
                    self.decoders.pop(ssrc, None)
                elif silence >= self.silence_seconds * 2:
                    self.buffers.pop(ssrc, None)
                    self.last_packet.pop(ssrc, None)
                    self.decoders.pop(ssrc, None)
        return result


def pcm_to_wav(pcm):
    source = tempfile.NamedTemporaryFile(suffix=".pcm", delete=False)
    target = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    source_path = source.name
    target_path = target.name
    source.write(pcm)
    source.close()
    target.close()
    try:
        subprocess.run(["ffmpeg", "-y", "-v", "error", "-f", "s16le", "-ar", "48000", "-ac", "2", "-i", source_path, "-ar", "16000", "-ac", "1", target_path], check=True, timeout=30, stdin=subprocess.DEVNULL)
    finally:
        try:
            os.unlink(source_path)
        except OSError:
            pass
    return target_path
