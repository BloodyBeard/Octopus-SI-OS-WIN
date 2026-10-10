#!/usr/bin/env python3
import asyncio
import base64
import hashlib
import json
import os
import re
import stat
import sys
import threading
import time
import urllib.parse
from contextlib import suppress

from octopus_content_hash import octopus_content_hash

from discord_boundary import (
    BLOBS_DIR,
    COMMAND_SYNC_PATH,
    INTERFACE_ID,
    MEDIA_DIR,
    VAR_DIR,
    BindingLink,
    GatewayHub,
    append_pending_attempt_evidence,
    approve_pairing,
    atomic_write_bytes,
    atomic_write_json,
    begin_pending_attempt,
    binding_by_interface,
    commit_cursor,
    commit_effect,
    content_hexdigest,
    ensure_private_binding,
    enqueue_pending_event,
    env_flag,
    env_snowflakes,
    external_error_observation,
    file_hexdigest,
    grant_media,
    hand_delivery_result,
    hand_failure,
    hand_json,
    hand_json_idempotent,
    is_duplicate_event,
    load_bounded_ids,
    load_config,
    load_cursor,
    load_effect,
    mark_thread_participated,
    materialize_payload,
    read_json,
    read_granted_media,
    read_own_outbound,
    reconcile_known_bindings,
    pending_events_for_replay,
    pairing_approved,
    read_bounded_regular,
    register_pairing,
    require_bool,
    require_int,
    require_keys,
    require_object,
    require_snowflake,
    require_string,
    reset_authoring_coordinator,
    revoke_pairing,
    resolve_base_url,
    resolve_proxy_url,
    resolve_token,
    sanitize_external_error,
    signal_ready,
    start_runtime_verdict_reader,
    stream_state,
    thread_participated,
    update_stream_state,
    fail_pending_attempt,
    complete_pending_event,
)


def snowflake_set(config_values, env_name):
    return set(config_values) | env_snowflakes(env_name)


def authorized(config, user, guild, channel, bot_mentioned=False):
    authz = config["authz"]
    user_id = str(getattr(user, "id", ""))
    is_bot = bool(getattr(user, "bot", False))
    if is_bot:
        mode = authz["allow_bots"]
        if mode == "none" or (mode == "mentions" and not bot_mentioned):
            return False
    guild_id = str(getattr(guild, "id", "")) if guild is not None else None
    channel_id = str(getattr(channel, "id", ""))
    parent_id = str(getattr(channel, "parent_id", "") or "")
    denied = snowflake_set(authz["denied_channel_ids"], authz["denied_channel_ids_env"])
    if channel_id in denied or parent_id in denied:
        return False
    guilds = snowflake_set(authz["allowed_guild_ids"], authz["allowed_guild_ids_env"])
    if guilds and guild_id not in guilds:
        return False
    channels = snowflake_set(authz["allowed_channel_ids"], authz["allowed_channel_ids_env"])
    if channels and channel_id not in channels and parent_id not in channels:
        return False
    if authz["allow_all_users"] or env_flag(authz["allow_all_users_env"]):
        return True
    users = snowflake_set(authz["allowed_user_ids"], authz["allowed_user_ids_env"])
    roles = snowflake_set(authz["allowed_role_ids"], authz["allowed_role_ids_env"])
    if user_id in users:
        return True
    user_roles = {str(getattr(role, "id", "")) for role in (getattr(user, "roles", None) or [])}
    if roles & user_roles:
        return True
    if user_id and pairing_approved(user_id):
        return True
    return not authz["deny_unmatched"]


def message_coordinates(channel):
    thread_id = None
    parent_id = getattr(channel, "parent_id", None)
    if parent_id is not None:
        thread_id = str(channel.id)
        return require_snowflake(str(parent_id), "discord message parent channel"), thread_id
    return require_snowflake(str(channel.id), "discord message channel"), None


def jsonable_user(user):
    return {
        "id": str(getattr(user, "id", "")),
        "name": getattr(user, "name", None),
        "display_name": getattr(user, "display_name", None),
        "global_name": getattr(user, "global_name", None),
        "bot": bool(getattr(user, "bot", False)),
        "role_ids": [str(getattr(role, "id")) for role in (getattr(user, "roles", None) or []) if getattr(role, "id", None) is not None],
    }


def split_message(text, limit):
    if not isinstance(text, str):
        raise ValueError("discord outbound text must be a string")
    if not text:
        return []
    chunks = []
    remaining = text
    while len(remaining) > limit:
        cut = remaining.rfind("\n", 0, limit + 1)
        if cut < limit // 2:
            cut = remaining.rfind(" ", 0, limit + 1)
        if cut < limit // 2:
            cut = limit
        chunks.append(remaining[:cut])
        remaining = remaining[cut:].lstrip("\n")
    if remaining:
        chunks.append(remaining)
    return chunks


def native_voice_metadata(pcm):
    import numpy

    samples = numpy.frombuffer(pcm, dtype=numpy.int16)
    frame_count = len(samples) // 2
    if frame_count == 0:
        raise ValueError("discord native voice audio decoded to no stereo frames")
    mono = samples[:frame_count * 2].reshape(-1, 2).astype(numpy.float32).mean(axis=1)
    edges = numpy.linspace(0, frame_count, 257, dtype=numpy.int64)
    levels = numpy.array([
        float(numpy.max(numpy.abs(mono[edges[index]:edges[index + 1]]))) if edges[index + 1] > edges[index] else 0.0
        for index in range(256)
    ], dtype=numpy.float32)
    peak = float(numpy.max(levels))
    waveform = numpy.zeros(256, dtype=numpy.uint8) if peak == 0 else numpy.rint(levels * (255.0 / peak)).clip(0, 255).astype(numpy.uint8)
    return frame_count / 48000.0, base64.b64encode(waveform.tobytes()).decode("ascii")


def safe_blob_path(blob_id, maximum):
    blob_id = require_string(blob_id, "discord blob_id")
    if not re.fullmatch(r"[0-9a-f]{64}", blob_id):
        raise ValueError("discord blob_id must be a lowercase Octopus content hash")
    payload = read_bounded_regular(os.path.join(BLOBS_DIR, blob_id), maximum, "outbound media blob")
    if octopus_content_hash(payload) != blob_id:
        raise ValueError("discord outbound media blob hash mismatch")
    path = os.path.join(MEDIA_DIR, f"outbound-{blob_id}")
    try:
        existing = read_bounded_regular(path, maximum, "private outbound media snapshot")
        if existing == payload:
            return path
    except FileNotFoundError:
        pass
    atomic_write_bytes(path, payload)
    return path


class InboundMediaTooLarge(ValueError):
    def __init__(self, observed):
        self.observed = observed
        super().__init__("discord inbound media exceeds max_download_bytes")


def private_media_evidence(path, maximum):
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise RuntimeError("discord cached media is not a regular file")
    if stat.S_IMODE(before.st_mode) != 0o600:
        raise RuntimeError("discord cached media must have mode 0600")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        current = os.fstat(descriptor)
        if not stat.S_ISREG(current.st_mode) or current.st_dev != before.st_dev or current.st_ino != before.st_ino:
            raise RuntimeError("discord cached media changed while opening")
        if current.st_size > maximum:
            raise InboundMediaTooLarge(current.st_size)
        digest = hashlib.sha256()
        observed = 0
        while observed < current.st_size:
            chunk = os.read(descriptor, min(65536, current.st_size - observed))
            if not chunk:
                raise RuntimeError("discord cached media ended before its recorded size")
            observed += len(chunk)
            digest.update(chunk)
        if os.read(descriptor, 1):
            raise RuntimeError("discord cached media changed while hashing")
        return observed, digest.hexdigest()
    finally:
        os.close(descriptor)


async def materialize_media_chunks(chunks, maximum, stem, extension):
    stem = re.sub(r"[^A-Za-z0-9._-]", "_", str(stem))[:160]
    extension = re.sub(r"[^A-Za-z0-9._-]", "_", str(extension))[:16]
    staged = os.path.join(MEDIA_DIR, f".attachment-{os.getpid()}.{time.time_ns()}.tmp")
    descriptor = os.open(staged, os.O_CREAT | os.O_EXCL | os.O_WRONLY | getattr(os, "O_NOFOLLOW", 0), 0o600)
    digest = hashlib.sha256()
    observed = 0
    try:
        with os.fdopen(descriptor, "wb") as sink:
            async for value in chunks:
                if not isinstance(value, (bytes, bytearray, memoryview)):
                    raise RuntimeError("discord media transport emitted a non-byte chunk")
                chunk = bytes(value)
                observed += len(chunk)
                if observed > maximum:
                    raise InboundMediaTooLarge(observed)
                sink.write(chunk)
                digest.update(chunk)
            sink.flush()
            os.fsync(sink.fileno())
        content_hash = digest.hexdigest()
        target = os.path.join(MEDIA_DIR, f"attachment-{stem}-{content_hash[:16]}{extension}")
        try:
            os.link(staged, target)
        except FileExistsError:
            existing_size, existing_hash = private_media_evidence(target, maximum)
            if existing_size != observed or existing_hash != content_hash:
                raise RuntimeError("discord cached media evidence is inconsistent")
        finally:
            try:
                os.unlink(staged)
            except FileNotFoundError:
                pass
        directory = os.open(MEDIA_DIR, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
        return target, observed, content_hash
    except Exception:
        try:
            os.unlink(staged)
        except FileNotFoundError:
            pass
        raise


def validate_embed(value, index):
    embed = require_keys(value, set(), {"title", "description", "url", "color", "timestamp", "footer", "image", "thumbnail", "author", "fields"}, f"discord embed[{index}]")
    if len(str(embed.get("title", ""))) > 256 or len(str(embed.get("description", ""))) > 4096:
        raise ValueError(f"discord embed[{index}] exceeds Discord limits")
    fields = embed.get("fields", [])
    if not isinstance(fields, list) or len(fields) > 25:
        raise ValueError(f"discord embed[{index}].fields is invalid")
    for field_index, field in enumerate(fields):
        field = require_keys(field, {"name", "value"}, {"inline"}, f"discord embed[{index}].fields[{field_index}]")
        if len(require_string(field["name"], "discord embed field name")) > 256 or len(require_string(field["value"], "discord embed field value")) > 1024:
            raise ValueError("discord embed field exceeds Discord limits")
        if "inline" in field:
            require_bool(field["inline"], "discord embed field inline")
    return embed


def validate_components(config, rows):
    if rows is None:
        return []
    if not isinstance(rows, list) or len(rows) > config["components"]["max_rows"]:
        raise ValueError("discord components rows exceed the declared limit")
    custom_ids = set()
    for row_index, row in enumerate(rows):
        if not isinstance(row, list) or not row or len(row) > config["components"]["max_items_per_row"]:
            raise ValueError(f"discord components row {row_index} is invalid")
        for item_index, item in enumerate(row):
            item = require_keys(item, {"type"}, {"label", "style", "custom_id", "url", "emoji", "disabled", "placeholder", "min_values", "max_values", "options"}, f"discord component[{row_index}][{item_index}]")
            if item["type"] not in ("button", "select"):
                raise ValueError("discord component type must be button or select")
            custom_id = item.get("custom_id")
            if item.get("url") is not None:
                if item["type"] != "button" or item.get("style") not in (None, "link") or custom_id is not None:
                    raise ValueError("discord link component must be a link button without custom_id")
                require_string(item["url"], "discord link button url")
            else:
                custom_id = require_string(custom_id, "discord component custom_id")
                if len(custom_id) > config["components"]["max_custom_id_len"] or custom_id in custom_ids:
                    raise ValueError("discord component custom_id is too long or repeated")
                custom_ids.add(custom_id)
            if item["type"] == "select":
                if item.get("url") is not None:
                    raise ValueError("discord select cannot carry a URL")
                options = item.get("options")
                if not isinstance(options, list) or not options or len(options) > 25:
                    raise ValueError("discord select options must contain 1 to 25 entries")
                for option in options:
                    require_keys(option, {"label", "value"}, {"description", "emoji", "default"}, "discord select option")
    return rows


def operation_request(config, body, binding, source_seq):
    body = require_object(body, "discord outbound payload")
    operation = require_string(body.get("operation"), "discord outbound operation")
    if operation not in config["outbound"]["allowed_operations"]:
        raise ValueError(f"discord outbound operation {operation!r} is not declared")
    target = body.get("target") or {}
    target = require_keys(target, set(), {"channel_id", "thread_id"}, "discord outbound target")
    expected_channel = binding["channel_id"]
    expected_thread = binding["thread_id"]
    channel_id = str(target.get("channel_id") or expected_channel)
    thread_id = target.get("thread_id", expected_thread)
    thread_id = None if thread_id is None else str(thread_id)
    if channel_id != expected_channel or thread_id != expected_thread:
        raise ValueError("discord outbound target escapes its private interface binding")
    request = dict(body)
    request["operation"] = operation
    request["target"] = {"channel_id": channel_id, "thread_id": thread_id}
    request["source_movement_seq"] = source_seq
    request["idempotency_key"] = hashlib.sha256(f"{INTERFACE_ID}:{source_seq}".encode("utf-8")).hexdigest()[:24]
    if "embeds" in request:
        embeds = request["embeds"]
        if not isinstance(embeds, list) or len(embeds) > 10:
            raise ValueError("discord outbound embeds must contain at most 10 entries")
        request["embeds"] = [validate_embed(value, index) for index, value in enumerate(embeds)]
    if "components" in request:
        request["components"] = validate_components(config, request["components"])
    attachments = request.get("attachments", [])
    if not isinstance(attachments, list) or len(attachments) > config["media"]["max_attachments_per_message"]:
        raise ValueError("discord outbound attachments exceed the declared limit")
    normalized = []
    total = 0
    for index, attachment in enumerate(attachments):
        attachment = require_keys(attachment, {"blob_id"}, {"filename", "description", "spoiler"}, f"discord attachment[{index}]")
        path = safe_blob_path(attachment["blob_id"], config["media"]["max_upload_bytes"])
        size = os.path.getsize(path)
        total += size
        if size > config["media"]["max_upload_bytes"] or total > config["media"]["max_upload_bytes"]:
            raise ValueError("discord outbound attachment bytes exceed max_upload_bytes")
        normalized.append({**attachment, "path": path})
    request["attachments"] = normalized
    native_voice = request.get("native_voice", False)
    require_bool(native_voice, "discord outbound native_voice")
    if native_voice:
        if operation != "message.create":
            raise ValueError("discord native_voice is only valid for message.create")
        if len(normalized) != 1:
            raise ValueError("discord native_voice requires exactly one attachment")
        if request.get("text") or request.get("embeds") or request.get("components") or request.get("reply_to_message_id") or request.get("tts"):
            raise ValueError("discord native_voice cannot carry text, embeds, components, reply, or TTS")
        filename = normalized[0].get("filename") or ""
        if not filename.lower().endswith((".ogg", ".oga")):
            raise ValueError("discord native_voice attachment filename must identify OGG Opus audio")
    if operation == "voice.play":
        audio_path = safe_blob_path(require_string(request.get("blob_id"), "discord voice.play blob_id"), config["media"]["max_upload_bytes"])
        if os.path.getsize(audio_path) > config["media"]["max_upload_bytes"]:
            raise ValueError("discord voice.play blob exceeds max_upload_bytes")
        request["audio_path"] = audio_path
    return request


def grant_payload_media(binding, payload):
    payload = json.loads(json.dumps(payload))
    items = list(payload.get("attachments") or [])
    if isinstance(payload.get("audio"), dict):
        items.append(payload["audio"])
    for item in items:
        path = item.pop("payload_path", None)
        if path is not None:
            item.update(grant_media(binding, path))
    return payload


def materialize_granted_media(config, link, payload):
    payload = json.loads(json.dumps(payload))
    event_key = hashlib.sha256(require_string(payload.get("event_id"), "discord binding media event_id").encode("utf-8")).hexdigest()[:24]
    items = list(payload.get("attachments") or [])
    if isinstance(payload.get("audio"), dict):
        items.append(payload["audio"])
    maximum = config["media"]["max_download_bytes"]
    frame_limit = config["driver_ipc"]["max_frame_bytes"]
    chunk_limit = min(1048576, max(1, ((frame_limit - 4096) * 3) // 4))
    for index, item in enumerate(items):
        handle = item.pop("media_handle", None)
        if handle is None:
            continue
        digest = require_string(item.pop("media_sha256", None), "discord binding media digest")
        size = require_int(item.pop("media_size", None), "discord binding media size", 0, maximum)
        token = require_string(item.pop("media_token", None), "discord binding media token")
        suffix = re.sub(r"[^A-Za-z0-9._-]", "_", os.path.splitext(item.get("filename") or handle)[1][:16])
        target = os.path.join(MEDIA_DIR, f"inbound-{event_key}-{index}-{digest[:16]}{suffix}")
        if os.path.isfile(target) and os.path.getsize(target) == size and file_hexdigest(target) == digest:
            item["payload_path"] = target
            item["sha256"] = digest
            continue
        staged = os.path.join(MEDIA_DIR, f".{os.path.basename(target)}.{os.getpid()}.{time.time_ns()}.tmp")
        offset = 0
        content_hash = hashlib.sha256()
        try:
            with open(staged, "xb") as sink:
                while offset < size:
                    response = link.request("media.read", {
                        "interface": INTERFACE_ID, "handle": handle, "sha256": digest,
                        "size": size, "token": token, "offset": offset, "limit": chunk_limit,
                    })
                    response = require_keys(response, {"offset", "data_base64", "eof"}, set(), "discord media.read response")
                    if require_int(response["offset"], "discord media.read offset", 0) != offset:
                        raise RuntimeError("discord media.read returned a discontinuous offset")
                    require_bool(response["eof"], "discord media.read eof")
                    try:
                        chunk = base64.b64decode(require_string(response["data_base64"], "discord media.read data", True), validate=True)
                    except ValueError as error:
                        raise RuntimeError("discord media.read returned malformed base64") from error
                    if not chunk or offset + len(chunk) > size:
                        raise RuntimeError("discord media.read returned an invalid chunk length")
                    sink.write(chunk)
                    content_hash.update(chunk)
                    offset += len(chunk)
                    if response["eof"] != (offset == size):
                        raise RuntimeError("discord media.read returned an invalid EOF boundary")
                sink.flush()
                os.fsync(sink.fileno())
            if content_hash.hexdigest() != digest:
                raise RuntimeError("discord media.read content hash mismatch")
            os.replace(staged, target)
            directory = os.open(MEDIA_DIR, os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            with suppress(FileNotFoundError):
                os.unlink(staged)
        item["payload_path"] = target
        item["sha256"] = digest
    return payload


def decode_output(config, label, payload, binding, source_seq):
    try:
        body = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"discord output is not JSON: {error}") from error
    if not isinstance(body, dict):
        raise ValueError("discord output must be a JSON object")
    outbound = config["outbound"]
    if label in outbound["turn_start_labels"]:
        return None, {"disposition": "turn_started", "label": label}, {}
    if label in outbound["telemetry_labels"]:
        return None, {"disposition": "observed", "label": label}, {}
    if label in outbound["stream_labels"]:
        state = stream_state()
        sequence = require_int(body.get("seq"), "discord stream seq", 0)
        text = body.get("text")
        if not isinstance(text, str):
            raise ValueError("discord stream delta has no text")
        previous = state["last_seq"]
        if previous is None and sequence != 0:
            raise ValueError("discord stream must begin at sequence zero")
        if previous is not None and sequence <= previous:
            return None, {"disposition": "duplicate_stream_delta", "seq": sequence}, {}
        if previous is not None and sequence != previous + 1:
            raise ValueError("discord stream sequence is discontinuous")
        accumulated = state["text"] + text
        state_updates = {"text": accumulated, "last_seq": sequence}
        now_ms = int(time.time() * 1000)
        if state["message_id"] and now_ms - state["last_edit_ms"] < outbound["min_edit_interval_ms"]:
            return None, {"disposition": "buffered_stream_delta", "seq": sequence}, state_updates
        operation = "message.edit" if state["message_id"] else "message.create"
        request = {"operation": operation, "text": accumulated, "stream": True, "final": False}
        if state["message_id"]:
            request["message_id"] = state["message_id"]
        return operation_request(config, request, binding, source_seq), {"disposition": "streamed", "seq": sequence}, state_updates
    if label in outbound["reply_labels"]:
        if label == "discord.outbound":
            request = operation_request(config, body, binding, source_seq)
        else:
            text = body.get("text")
            if not isinstance(text, str) or not text:
                raise ValueError("discord final provider output has no text")
            state = stream_state()
            request = {"operation": "message.edit" if state["message_id"] else "message.create", "text": text, "final": True}
            if state["message_id"]:
                request["message_id"] = state["message_id"]
            request = operation_request(config, request, binding, source_seq)
        return request, {"disposition": "delivered", "label": label}, {"text": "", "last_seq": None}
    if label in outbound["error_labels"]:
        detail = body.get("message") or body.get("error") or body.get("detail") or json.dumps(body, ensure_ascii=False)
        state = stream_state()
        request = {"operation": "message.edit" if state["message_id"] else "message.create", "text": f"Request failed: {detail}", "final": True, "error": True}
        if state["message_id"]:
            request["message_id"] = state["message_id"]
        return operation_request(config, request, binding, source_seq), {"disposition": "error_delivered", "label": label}, {"text": "", "last_seq": None}
    raise ValueError(f"discord interface does not declare outbound label {label!r}")


class GatewayDriver:
    def __init__(self, config, token):
        self.config = config
        self.token = token
        self.discord = None
        self.client = None
        self.loop = None
        self.hub = None
        self.ready = asyncio.Event()
        self.pending_interactions = {}
        self.typing_tasks = {}
        self.turn_sources = {}
        self.batch_payloads = {}
        self.batch_tasks = {}
        self.batch_locks = [asyncio.Lock() for _ in range(self.config["gating"]["batch_lock_stripes"])]
        self.in_flight_events = set()
        self.voice_clients = {}
        self.voice_receivers = {}
        self.voice_tasks = {}
        self.voice_mixers = {}
        self.voice_bindings = {}
        self.voice_timeouts = {}
        self.gateway_started = False
        self.delivery_failure = None

    async def run(self):
        try:
            import discord
            from discord.ext import commands
        except ImportError as error:
            raise RuntimeError("world_discord requires its private requirements.txt dependencies") from error
        self.discord = discord
        discord.http.Route.BASE = resolve_base_url(self.config)
        self.loop = asyncio.get_running_loop()
        gateway = self.config["gateway"]
        intents = discord.Intents.default()
        intents.message_content = gateway["message_content_intent"]
        intents.members = gateway["members_intent"]
        intents.voice_states = gateway["voice_states_intent"]
        intents.presences = gateway["presences_intent"]
        mentions = self.config["outbound"]["allowed_mentions"]
        allowed_mentions = discord.AllowedMentions(everyone=mentions["everyone"], roles=mentions["roles"], users=mentions["users"], replied_user=mentions["replied_user"])
        proxy = resolve_proxy_url(self.config)
        self.client = commands.Bot(command_prefix="!", intents=intents, allowed_mentions=allowed_mentions, proxy=proxy)
        self.client.http.max_ratelimit_timeout = self.config["outbound"]["rate_limit"]["max_retry_after_ms"] / 1000.0
        self.hub = GatewayHub(self.config, self._outbound_from_peer)
        driver = self

        @self.client.event
        async def on_ready():
            atomic_write_json(os.path.join(VAR_DIR, "gateway-reconnect.json"), {"schema_version": 1, "backoff_ms": driver.config["discord_api"]["reconnect_backoff_ms"]})
            driver.ready.set()
            await driver._sync_commands()

        @self.client.event
        async def on_message(message):
            await driver._message(message)

        @self.client.event
        async def on_message_edit(before, after):
            if before.content != after.content or before.attachments != after.attachments:
                await driver._message(after, event_type="message.update")

        @self.client.event
        async def on_raw_message_delete(payload):
            await driver._raw_message_delete(payload)

        @self.client.event
        async def on_raw_reaction_add(payload):
            await driver._raw_reaction(payload, "reaction.add")

        @self.client.event
        async def on_raw_reaction_remove(payload):
            await driver._raw_reaction(payload, "reaction.remove")

        @self.client.event
        async def on_interaction(interaction):
            await driver._interaction(interaction)

        @self.client.event
        async def on_thread_create(thread):
            await driver._thread_event(thread, "thread.create")

        @self.client.event
        async def on_thread_update(before, after):
            await driver._thread_event(after, "thread.update")

        @self.client.event
        async def on_thread_delete(thread):
            await driver._thread_event(thread, "thread.delete")

        status = getattr(discord.Status, gateway["status"])
        activity = discord.Game(gateway["activity"]) if gateway["activity"] else None
        task = None
        try:
            reset_authoring_coordinator()
            await asyncio.to_thread(reconcile_known_bindings, self.config, self.hub)
            await self._replay_pending_events()
            task = asyncio.create_task(self.client.start(self.token, reconnect=True))
            await asyncio.wait_for(self.ready.wait(), self.config["discord_api"]["ready_timeout_ms"] / 1000.0)
            await self.client.change_presence(status=status, activity=activity)
            sys.stderr.write(f"discord gateway up: interface={INTERFACE_ID} user={self.client.user.id} driver_ipc={self.hub.host}:{self.hub.port}\n")
            sys.stderr.flush()
            self.gateway_started = True
            self.delivery_failure = self.loop.create_future()
            done, _ = await asyncio.wait({task, self.delivery_failure}, return_when=asyncio.FIRST_COMPLETED)
            if self.delivery_failure in done:
                raise self.delivery_failure.exception()
            await task
        finally:
            self.gateway_started = False
            if task is not None and not task.done():
                task.cancel()
                with suppress(Exception, asyncio.CancelledError):
                    await task
            self.hub.close()

    def _configured_commands(self):
        option_types = {"string": 3, "integer": 4, "boolean": 5, "user": 6, "channel": 7, "role": 8, "mentionable": 9, "number": 10, "attachment": 11}
        payloads = []
        for command in self.config["commands"]["commands"]:
            options = []
            for option in command["options"]:
                item = {"type": option_types[option["type"]], "name": option["name"], "description": option["description"], "required": option["required"]}
                for key in ("choices", "min_value", "max_value", "min_length", "max_length", "channel_types"):
                    if key in option:
                        item[key] = option[key]
                options.append(item)
            payload = {"type": 1, "name": command["name"], "description": command["description"], "dm_permission": command["dm_permission"], "options": options}
            if command["default_member_permissions"] is not None:
                payload["default_member_permissions"] = str(command["default_member_permissions"])
            payloads.append(payload)
        return payloads

    def _canonical_command(self, payload):
        return {
            "type": int(payload.get("type", 1)),
            "name": payload.get("name"),
            "description": payload.get("description", ""),
            "dm_permission": bool(payload.get("dm_permission", True)),
            "default_member_permissions": None if payload.get("default_member_permissions") is None else str(payload.get("default_member_permissions")),
            "options": payload.get("options") or [],
        }

    async def _sync_commands(self):
        policy = self.config["commands"]["sync_policy"]
        if policy == "off":
            return
        desired = self._configured_commands()
        fingerprint = hashlib.sha256(json.dumps(desired, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()
        state = read_json(COMMAND_SYNC_PATH, {"schema_version": 1})
        if state.get("schema_version") != 1:
            raise RuntimeError("discord command sync state schema_version is invalid")
        retry_after_until = state.get("retry_after_until", 0)
        if isinstance(retry_after_until, (int, float)) and retry_after_until > time.time():
            return
        if state.get("fingerprint") == fingerprint and state.get("last_success_at"):
            return
        application_id = self.client.application_id or self.client.user.id
        route = self.discord.http.Route
        try:
            if policy == "bulk":
                result = await self.client.http.request(route("PUT", "/applications/{application_id}/commands", application_id=application_id), json=desired)
                summary = {"total": len(result), "bulk": True}
            else:
                existing = await self.client.http.request(route("GET", "/applications/{application_id}/commands", application_id=application_id))
                desired_map = {(item["type"], item["name"]): item for item in desired}
                existing_map = {(int(item.get("type", 1)), item["name"]): item for item in existing}
                summary = {"total": len(desired), "unchanged": 0, "updated": 0, "created": 0, "deleted": 0}
                interval = self.config["commands"]["mutation_interval_ms"] / 1000.0
                mutations = 0

                async def mutate(method, path, **params):
                    nonlocal mutations
                    if mutations and interval:
                        await asyncio.sleep(interval)
                    payload = params.pop("payload", None)
                    result = await self.client.http.request(route(method, path, **params), json=payload)
                    mutations += 1
                    return result

                for key in sorted(set(existing_map) - set(desired_map)):
                    current = existing_map[key]
                    await mutate("DELETE", "/applications/{application_id}/commands/{command_id}", application_id=application_id, command_id=current["id"])
                    summary["deleted"] += 1
                for key, wanted in desired_map.items():
                    current = existing_map.get(key)
                    if current is None:
                        await mutate("POST", "/applications/{application_id}/commands", application_id=application_id, payload=wanted)
                        summary["created"] += 1
                    elif self._canonical_command(current) == self._canonical_command(wanted):
                        summary["unchanged"] += 1
                    else:
                        await mutate("PATCH", "/applications/{application_id}/commands/{command_id}", application_id=application_id, command_id=current["id"], payload=wanted)
                        summary["updated"] += 1
            atomic_write_json(COMMAND_SYNC_PATH, {"schema_version": 1, "fingerprint": fingerprint, "last_success_at": time.time(), "summary": summary}, indent=2)
        except (self.discord.HTTPException, self.discord.RateLimited) as error:
            if isinstance(error, self.discord.RateLimited) or getattr(error, "status", None) == 429:
                retry_after = min(float(getattr(error, "retry_after", 30)), self.config["outbound"]["rate_limit"]["max_retry_after_ms"] / 1000.0)
                atomic_write_json(COMMAND_SYNC_PATH, {"schema_version": 1, "fingerprint": fingerprint, "last_attempt_at": time.time(), "retry_after_until": time.time() + retry_after}, indent=2)
                return
            raise

    def _already_seen(self, identifier):
        return str(identifier) in load_bounded_ids(os.path.join(VAR_DIR, "gateway-dedup.json"), "discord gateway dedup", 10000)

    def _reconcile_delivered_entry(self, entry):
        for source_event_id in entry["source_event_ids"]:
            if not self._already_seen(source_event_id):
                is_duplicate_event(source_event_id)

    async def _replay_pending_events(self):
        for entry in pending_events_for_replay(self.config):
            if entry["status"] == "delivered":
                self._reconcile_delivered_entry(entry)
                continue
            await self._deliver_pending_event(entry["event_id"])

    async def _deliver_pending_event(self, event_id):
        journal = self.config["inbound_journal"]
        last_error = None
        for local_attempt in range(journal["delivery_max_attempts"]):
            entries = {entry["event_id"]: entry for entry in pending_events_for_replay(self.config)}
            current = entries.get(event_id)
            if current is None:
                raise RuntimeError(f"discord pending event {event_id!r} disappeared")
            if current["status"] == "delivered":
                self._reconcile_delivered_entry(current)
                return None
            retry_at = current.get("next_retry_at_unix_ms", 0)
            now_ms = int(time.time() * 1000)
            if retry_at > now_ms:
                await asyncio.sleep((retry_at - now_ms) / 1000.0)
            entry = await asyncio.to_thread(begin_pending_attempt, self.config, event_id)

            def evidence(phase, outcome, details):
                append_pending_attempt_evidence(self.config, event_id, phase, outcome, details)

            try:
                binding = await asyncio.to_thread(
                    ensure_private_binding,
                    self.config,
                    self.hub,
                    entry["channel_id"],
                    entry["thread_id"],
                    evidence,
                )
                payload = await asyncio.to_thread(grant_payload_media, binding, entry["payload"])
                await asyncio.to_thread(evidence, "private_delivery", "prepared", {"binding_interface": binding["interface"], "payload_sha256": entry["payload_sha256"]})
                admitted = await asyncio.to_thread(self.hub.dispatch, binding, payload)
                await asyncio.to_thread(evidence, "private_delivery", "admitted", {"binding_interface": binding["interface"], "movement_seq": admitted})
                completed = await asyncio.to_thread(complete_pending_event, self.config, event_id, binding["interface"], admitted)
                self._reconcile_delivered_entry(completed)
                return binding, admitted
            except Exception as error:
                last_error = error
                delay = min(journal["delivery_retry_backoff_ms"] * (2 ** local_attempt), journal["delivery_retry_backoff_cap_ms"])
                await asyncio.to_thread(fail_pending_attempt, self.config, event_id, error, delay)
                if local_attempt + 1 < journal["delivery_max_attempts"]:
                    await asyncio.sleep(delay / 1000.0)
        failure = RuntimeError(f"discord pending event {event_id!r} remains durable after delivery failure: {sanitize_external_error(self.config, last_error)}")
        if self.gateway_started and self.delivery_failure is not None and not self.delivery_failure.done():
            self.delivery_failure.set_exception(failure)
        raise failure

    async def _ensure_dispatch(self, channel_id, thread_id, payload, event_id, source_event_ids=None):
        event_id = require_string(str(event_id), "discord event id")
        if self._already_seen(event_id) or event_id in self.in_flight_events:
            return None
        self.in_flight_events.add(event_id)
        try:
            source_event_ids = source_event_ids or [event_id]
            entry, _ = await asyncio.to_thread(enqueue_pending_event, self.config, channel_id, thread_id, payload, event_id, source_event_ids)
            if entry["status"] == "delivered":
                self._reconcile_delivered_entry(entry)
                return None
            return await self._deliver_pending_event(event_id)
        finally:
            self.in_flight_events.discard(event_id)

    async def _download_attachment(self, attachment):
        declared = getattr(attachment, "size", None)
        maximum = self.config["media"]["max_download_bytes"]
        if declared is not None and int(declared) > maximum:
            return {"id": str(attachment.id), "filename": attachment.filename, "content_type": attachment.content_type, "size": int(declared), "disposition": "too_large"}
        source_url = require_string(str(getattr(attachment, "proxy_url", None) or attachment.url), "discord attachment URL")
        parsed = urllib.parse.urlsplit(source_url)
        if parsed.scheme != "https" or not parsed.hostname or parsed.username is not None or parsed.password is not None:
            raise ValueError("discord attachment URL must be a credential-free HTTPS URL")
        durable_source_url = urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, parsed.path, "", ""))
        extension = re.sub(r"[^A-Za-z0-9._-]", "_", os.path.splitext(attachment.filename or "")[1][:16])
        try:
            import aiohttp
            timeout = aiohttp.ClientTimeout(total=self.config["discord_api"]["ready_timeout_ms"] / 1000.0)
            async with aiohttp.ClientSession(timeout=timeout, trust_env=False) as session:
                async with session.get(source_url, proxy=resolve_proxy_url(self.config), allow_redirects=True) as response:
                    if response.status < 200 or response.status >= 300:
                        raise RuntimeError(f"discord attachment transport returned HTTP {response.status}")
                    final_url = urllib.parse.urlsplit(str(response.url))
                    if final_url.scheme != "https" or not final_url.hostname:
                        raise RuntimeError("discord attachment transport redirected outside HTTPS")
                    content_length = response.headers.get("Content-Length")
                    if content_length is not None:
                        try:
                            response_size = int(content_length)
                        except ValueError as error:
                            raise RuntimeError("discord attachment transport returned malformed Content-Length") from error
                        if response_size > maximum:
                            return {"id": str(attachment.id), "filename": attachment.filename, "content_type": attachment.content_type, "size": response_size, "disposition": "too_large"}
                    path, size, digest = await materialize_media_chunks(response.content.iter_chunked(65536), maximum, attachment.id, extension)
        except InboundMediaTooLarge as error:
            return {"id": str(attachment.id), "filename": attachment.filename, "content_type": attachment.content_type, "size": error.observed, "disposition": "too_large"}
        except Exception as error:
            raise RuntimeError(f"discord attachment download failed: {type(error).__name__}") from None
        voice_marker = getattr(attachment, "is_voice_message", False)
        voice_message = bool(voice_marker() if callable(voice_marker) else voice_marker)
        item = {"id": str(attachment.id), "filename": attachment.filename, "content_type": attachment.content_type or "application/octet-stream", "size": size, "payload_path": path, "sha256": digest, "url": durable_source_url, "description": getattr(attachment, "description", None), "ephemeral": bool(getattr(attachment, "ephemeral", False)), "voice_message": voice_message}
        if extension.lower() in set(self.config["media"]["inline_text_extensions"]) and size <= self.config["media"]["inline_text_max_bytes"]:
            try:
                with open(path, "rb") as handle:
                    item["text"] = handle.read(self.config["media"]["inline_text_max_bytes"] + 1).decode("utf-8")
            except UnicodeDecodeError:
                pass
        return item

    async def _history(self, channel, before):
        if not self.config["gating"]["history_backfill"]:
            return []
        limit = self.config["gating"]["history_backfill_limit"]
        if limit <= 0:
            return []
        items = []
        async for message in channel.history(limit=limit, before=before, oldest_first=False):
            if message.author == self.client.user:
                break
            if message.type not in (self.discord.MessageType.default, self.discord.MessageType.reply):
                continue
            items.append({"message_id": str(message.id), "author": jsonable_user(message.author), "text": message.clean_content or message.content or "", "attachment_count": len(message.attachments), "created_at": message.created_at.isoformat()})
        items.reverse()
        return items

    async def _message(self, message, event_type="message.create"):
        if message.author == self.client.user or message.type not in (self.discord.MessageType.default, self.discord.MessageType.reply):
            return
        mentioned = self.client.user in message.mentions if self.client.user else False
        other_bots_mentioned = any(getattr(user, "bot", False) and user != self.client.user for user in message.mentions)
        if other_bots_mentioned and not mentioned:
            return
        if not authorized(self.config, message.author, message.guild, message.channel, mentioned):
            await self._unauthorized(message)
            return
        is_dm = message.guild is None
        if is_dm and not self.config["gating"]["respond_in_dms"]:
            return
        channel_id, thread_id = message_coordinates(message.channel)
        channel_ids = {str(message.channel.id), channel_id}
        free = snowflake_set(self.config["gating"]["free_response_channel_ids"], self.config["gating"]["free_response_channel_ids_env"])
        free_response = bool(channel_ids & free)
        participated = thread_id is not None and thread_participated(thread_id) and not self.config["gating"]["thread_require_mention"]
        replied_to_bot = bool(message.reference and getattr(message.reference, "resolved", None) and getattr(message.reference.resolved, "author", None) == self.client.user)
        if not is_dm and self.config["gating"]["require_mention_in_guilds"] and not free_response and not participated and not mentioned and not (replied_to_bot and self.config["gating"]["reply_to_bot_counts_as_mention"]):
            return
        effective = message.channel
        if not is_dm and thread_id is None and self.config["gating"]["auto_thread"] and mentioned:
            no_threads = snowflake_set(self.config["gating"]["no_thread_channel_ids"], self.config["gating"]["no_thread_channel_ids_env"])
            if channel_id not in no_threads and not free_response and message.type != self.discord.MessageType.reply:
                name = re.sub(r"<[@#][!&]?\d+>", "", message.clean_content or message.content or "").strip() or "Octopus"
                name = name[:100]
                try:
                    effective = await message.create_thread(name=name, auto_archive_duration=self.config["gating"]["auto_thread_archive_minutes"])
                except Exception:
                    seed = await message.channel.send(f"Thread: {name}")
                    effective = await seed.create_thread(name=name, auto_archive_duration=self.config["gating"]["auto_thread_archive_minutes"])
                channel_id, thread_id = message_coordinates(effective)
                mark_thread_participated(thread_id)
        raw_attachments = list(message.attachments)
        snapshot_text = []
        for snapshot in getattr(message, "message_snapshots", None) or []:
            if getattr(snapshot, "content", None):
                snapshot_text.append(snapshot.content)
            raw_attachments.extend(getattr(snapshot, "attachments", None) or [])
        resolved_reference = getattr(getattr(message, "reference", None), "resolved", None)
        raw_attachments.extend(getattr(resolved_reference, "attachments", None) or [])
        unique_attachments = {}
        for attachment in raw_attachments:
            unique_attachments[str(attachment.id)] = attachment
        attachments = []
        for attachment in unique_attachments.values():
            attachments.append(await self._download_attachment(attachment))
        reference = None
        if message.reference:
            resolved = getattr(message.reference, "resolved", None)
            reference = {"message_id": str(message.reference.message_id), "text": getattr(resolved, "content", None), "author": jsonable_user(resolved.author) if getattr(resolved, "author", None) else None}
        history = [] if effective is not message.channel else await self._history(message.channel, message)
        text = message.content or message.clean_content or ""
        if not text and snapshot_text:
            text = "\n".join(snapshot_text)
        if mentioned and self.client.user:
            text = re.sub(rf"<@!?{self.client.user.id}>", "", text).strip()
        event_id = str(message.id) if event_type == "message.create" else f"{event_type}:{message.id}:{getattr(message, 'edited_at', None)}"
        payload = {
            "source": "discord", "event_type": event_type, "event_id": event_id,
            "message_id": str(message.id), "channel_id": channel_id, "thread_id": thread_id,
            "parent_channel_id": str(getattr(effective, "parent_id", "") or "") or None,
            "guild": None if message.guild is None else {"id": str(message.guild.id), "name": message.guild.name},
            "channel": {"id": str(effective.id), "name": getattr(effective, "name", None), "topic": getattr(effective, "topic", None), "type": str(getattr(effective, "type", "unknown"))},
            "author": jsonable_user(message.author), "text": text,
            "attachments": attachments, "embeds": [embed.to_dict() for embed in message.embeds],
            "stickers": [{"id": str(sticker.id), "name": sticker.name, "url": str(sticker.url)} for sticker in message.stickers],
            "mentions": {"users": [str(user.id) for user in message.mentions], "roles": [str(role.id) for role in message.role_mentions], "everyone": message.mention_everyone},
            "reply_to": reference, "history": history, "created_at": message.created_at.isoformat(),
        }
        if not text and not attachments and not message.embeds and not message.stickers:
            return
        if thread_id:
            mark_thread_participated(thread_id)
        await self._queue_or_dispatch(channel_id, thread_id, payload, event_id, plain=bool(text and not attachments and event_type == "message.create"))

    async def _queue_or_dispatch(self, channel_id, thread_id, payload, event_id, plain):
        delay_ms = self.config["gating"]["text_batch_delay_ms"]
        if not plain or delay_ms <= 0:
            result = await self._ensure_dispatch(channel_id, thread_id, payload, event_id)
            if result:
                await self._processing_started(result[0], payload)
            return
        key = f"{channel_id}:{thread_id or ''}:{payload['author']['id']}"
        lock = self._batch_lock(key)
        async with lock:
            existing = self.batch_payloads.get(key)
            if self._already_seen(event_id) or (existing is not None and event_id in existing["event_ids"]):
                return
            maximum_events = self.config["gating"]["max_batch_events"]
            maximum_bytes = self.config["gating"]["max_batch_bytes"]
            event_ids = [event_id]
            candidate = payload
            if existing is not None:
                event_ids = existing["event_ids"] + [event_id]
                candidate = dict(existing["payload"])
                candidate["text"] = existing["payload"]["text"] + "\n" + payload["text"]
                candidate["message_id"] = payload["message_id"]
                candidate["event_id"] = payload["event_id"]
                candidate["created_at"] = payload["created_at"]
                if len(event_ids) > maximum_events or self._batch_size_bytes(candidate, event_ids) > maximum_bytes:
                    await self._flush_batch_locked(key)
                    existing = None
                    event_ids = [event_id]
                    candidate = payload
            byte_count = self._batch_size_bytes(candidate, event_ids)
            if byte_count > maximum_bytes:
                result = await self._ensure_dispatch(channel_id, thread_id, payload, event_id)
                if result:
                    await self._processing_started(result[0], payload)
                return
            if existing is None:
                delay = self.config["gating"]["text_batch_split_delay_ms"] if len(payload["text"]) >= self.config["gating"]["text_batch_split_threshold"] else delay_ms
                self.batch_payloads[key] = {
                    "payload": payload,
                    "event_ids": [event_id],
                    "channel_id": channel_id,
                    "thread_id": thread_id,
                    "byte_count": byte_count,
                    "delay_ms": delay,
                }
                self.batch_tasks[key] = asyncio.create_task(self._flush_batch(key))
                return
            existing["payload"] = candidate
            existing["event_ids"] = event_ids
            existing["byte_count"] = byte_count

    def _batch_lock(self, key):
        digest = hashlib.sha256(key.encode("utf-8")).digest()
        return self.batch_locks[int.from_bytes(digest[:8], "big") % len(self.batch_locks)]

    def _batch_size_bytes(self, payload, event_ids):
        body = {"payload": payload, "event_ids": event_ids}
        return len(json.dumps(body, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8"))

    async def _dispatch_batch(self, batch):
        composite = "messages:" + hashlib.sha256("|".join(batch["event_ids"]).encode("utf-8")).hexdigest()
        result = await asyncio.shield(self._ensure_dispatch(batch["channel_id"], batch["thread_id"], batch["payload"], composite, batch["event_ids"]))
        if result:
            await self._processing_started(result[0], batch["payload"])

    async def _flush_batch_locked(self, key):
        batch = self.batch_payloads.pop(key, None)
        task = self.batch_tasks.pop(key, None)
        current = asyncio.current_task()
        if task is not None and task is not current and not task.done():
            task.cancel()
            with suppress(Exception, asyncio.CancelledError):
                await task
        if batch is not None:
            await self._dispatch_batch(batch)

    async def _flush_batch(self, key):
        current = asyncio.current_task()
        try:
            await asyncio.sleep(self.batch_payloads[key]["delay_ms"] / 1000.0)
            async with self._batch_lock(key):
                if self.batch_tasks.get(key) is not current:
                    return
                self.batch_tasks.pop(key, None)
                batch = self.batch_payloads.pop(key)
                await self._dispatch_batch(batch)
        except asyncio.CancelledError:
            pass
        finally:
            if self.batch_tasks.get(key) is current:
                self.batch_tasks.pop(key, None)

    async def _unauthorized(self, message):
        pairing = self.config["authz"]["pairing"]
        if not pairing["enabled"] or (pairing["dm_only"] and message.guild is not None):
            return
        code = register_pairing(str(message.author.id))
        await message.channel.send(pairing["prompt"].format(code=code))

    async def _processing_started(self, binding, payload):
        self.turn_sources[binding["interface"]] = {"message_id": payload.get("message_id"), "target_id": binding["thread_id"] or binding["channel_id"]}
        reactions = self.config["outbound"]["reactions"]
        if reactions["enabled"] and payload.get("message_id"):
            with suppress(Exception):
                channel = await self._channel(binding["thread_id"] or binding["channel_id"])
                message = await channel.fetch_message(int(payload["message_id"]))
                await message.add_reaction(reactions["in_progress"])
        if self.config["outbound"]["typing"]["enabled"]:
            await self._start_typing(binding["interface"], binding["thread_id"] or binding["channel_id"])

    async def _channel(self, identifier):
        channel = self.client.get_channel(int(identifier))
        if channel is None:
            channel = await self.client.fetch_channel(int(identifier))
        if channel is None:
            raise RuntimeError(f"Discord channel {identifier} was not found")
        return channel


    async def _raw_message_delete(self, raw):
        channel = await self._channel(raw.channel_id)
        guild = self.client.get_guild(raw.guild_id) if raw.guild_id else None
        user = getattr(raw.cached_message, "author", None) if raw.cached_message else None
        if user is not None and not authorized(self.config, user, guild, channel):
            return
        channel_id, thread_id = message_coordinates(channel)
        binding = next((item for item in load_bindings_safe() if item["channel_id"] == channel_id and item["thread_id"] == thread_id), None)
        if binding is None:
            return
        event_id = f"delete:{raw.channel_id}:{raw.message_id}"
        payload = {"source": "discord", "event_type": "message.delete", "event_id": event_id, "message_id": str(raw.message_id), "channel_id": channel_id, "thread_id": thread_id, "guild_id": None if raw.guild_id is None else str(raw.guild_id)}
        if not self._already_seen(event_id):
            await asyncio.to_thread(self.hub.dispatch, binding, payload)
            is_duplicate_event(event_id)


    async def _raw_reaction(self, raw, event_type):
        if self.client.user and raw.user_id == self.client.user.id:
            return
        channel = await self._channel(raw.channel_id)
        guild = self.client.get_guild(raw.guild_id) if raw.guild_id else None
        user = raw.member
        if user is None and guild is not None:
            user = guild.get_member(raw.user_id)
        if user is None:
            with suppress(Exception):
                user = await self.client.fetch_user(raw.user_id)
        if user is None or not authorized(self.config, user, guild, channel):
            return
        channel_id, thread_id = message_coordinates(channel)
        binding = next((item for item in load_bindings_safe() if item["channel_id"] == channel_id and item["thread_id"] == thread_id), None)
        if binding is None:
            return
        emoji = {"id": None if raw.emoji.id is None else str(raw.emoji.id), "name": raw.emoji.name, "animated": raw.emoji.animated}
        event_id = f"{event_type}:{raw.channel_id}:{raw.message_id}:{raw.user_id}:{raw.emoji}"
        payload = {"source": "discord", "event_type": event_type, "event_id": event_id, "message_id": str(raw.message_id), "channel_id": channel_id, "thread_id": thread_id, "guild_id": None if raw.guild_id is None else str(raw.guild_id), "author": jsonable_user(user), "emoji": emoji}
        if not self._already_seen(event_id):
            await asyncio.to_thread(self.hub.dispatch, binding, payload)
            is_duplicate_event(event_id)


    async def _thread_event(self, thread, event_type):
        if getattr(thread, "parent_id", None) is None:
            return
        channel_id, thread_id = message_coordinates(thread)
        event_id = f"{event_type}:{thread.id}:{getattr(thread, 'archive_timestamp', None)}"
        payload = {
            "source": "discord", "event_type": event_type, "event_id": event_id,
            "channel_id": channel_id, "thread_id": thread_id,
            "guild_id": str(thread.guild.id), "name": thread.name,
            "archived": bool(thread.archived), "locked": bool(thread.locked),
            "auto_archive_duration": thread.auto_archive_duration,
            "owner_id": None if thread.owner_id is None else str(thread.owner_id),
        }
        binding = next((item for item in load_bindings_safe() if item["channel_id"] == channel_id and item["thread_id"] == thread_id), None)
        if binding is None:
            return
        if not self._already_seen(event_id):
            await asyncio.to_thread(self.hub.dispatch, binding, payload)
            is_duplicate_event(event_id)


    def _interaction_options(self, interaction):
        data = interaction.data or {}
        options = {}
        for option in data.get("options") or []:
            name = option.get("name")
            if isinstance(name, str):
                options[name] = option.get("value")
        return options


    async def _interaction(self, interaction):
        if interaction.type not in (self.discord.InteractionType.application_command, self.discord.InteractionType.component, self.discord.InteractionType.modal_submit):
            return
        if not authorized(self.config, interaction.user, interaction.guild, interaction.channel):
            if not interaction.response.is_done():
                await interaction.response.send_message("You are not authorized to use this Discord surface.", ephemeral=True)
            return
        if not interaction.response.is_done():
            await interaction.response.defer(ephemeral=False)
        channel_id, thread_id = message_coordinates(interaction.channel)
        interaction_id = str(interaction.id)
        self.pending_interactions[interaction_id] = (time.monotonic(), interaction)
        cutoff = time.monotonic() - self.config["components"]["pending_ttl_seconds"]
        self.pending_interactions = {key: value for key, value in self.pending_interactions.items() if value[0] >= cutoff}
        data = interaction.data or {}
        event_type = "command" if interaction.type == self.discord.InteractionType.application_command else "component"
        payload = {
            "source": "discord", "event_type": event_type, "event_id": interaction_id,
            "interaction_id": interaction_id, "interaction_type": int(interaction.type.value),
            "channel_id": channel_id, "thread_id": thread_id,
            "guild": None if interaction.guild is None else {"id": str(interaction.guild.id), "name": interaction.guild.name},
            "author": jsonable_user(interaction.user), "command": data.get("name"),
            "options": self._interaction_options(interaction), "custom_id": data.get("custom_id"),
            "values": data.get("values") or [], "message_id": None if interaction.message is None else str(interaction.message.id),
        }
        result = await self._ensure_dispatch(channel_id, thread_id, payload, interaction_id)
        if result:
            await self._processing_started(result[0], payload)


    def _outbound_from_peer(self, operation, payload):
        payload = require_object(payload, "discord IPC outbound payload")
        if operation == "media.read":
            payload = require_keys(payload, {"interface", "handle", "sha256", "size", "token", "offset", "limit"}, set(), "discord IPC media.read payload")
            return read_granted_media(
                payload.get("interface"), payload.get("handle"), payload.get("sha256"),
                payload.get("size"), payload.get("token"), payload.get("offset"), payload.get("limit"),
            )
        if operation != "outbound":
            raise RuntimeError(f"discord gateway does not accept IPC operation {operation!r}")
        payload = require_keys(payload, {"interface", "request"}, set(), "discord IPC outbound payload")
        interface = require_string(payload.get("interface"), "discord IPC outbound interface")
        binding = binding_by_interface(interface)
        if binding is None:
            raise RuntimeError("discord IPC outbound interface has no durable binding")
        request = require_object(payload.get("request"), "discord IPC outbound request")
        future = asyncio.run_coroutine_threadsafe(self._execute(binding, request), self.loop)
        return future.result(self.config["driver_ipc"]["request_timeout_ms"] / 1000.0)


    async def _execute(self, binding, request):
        return await self._execute_once(binding, request)

    async def _execute_once(self, binding, request):
        operation = request["operation"]
        if operation == "message.create":
            result = await self._message_create(binding, request)
        elif operation == "message.edit":
            result = await self._message_edit(binding, request)
        elif operation == "message.delete":
            result = await self._message_delete(binding, request)
        elif operation.startswith("reaction."):
            result = await self._reaction(binding, request)
        elif operation == "typing.start":
            await self._start_typing(binding["interface"], binding["thread_id"] or binding["channel_id"])
            result = {"typing": "started"}
        elif operation == "typing.stop":
            await self._stop_typing(binding["interface"])
            result = {"typing": "stopped"}
        elif operation == "thread.create":
            result = await self._thread_create(binding, request)
        elif operation == "thread.edit":
            result = await self._thread_edit(binding, request)
        elif operation == "interaction.respond":
            result = await self._interaction_respond(request)
        elif operation == "voice.join":
            result = await self._voice_join(binding, request)
        elif operation == "voice.leave":
            result = await self._voice_leave(binding, request)
        elif operation == "voice.play":
            result = await self._voice_play(binding, request)
        else:
            raise ValueError(f"discord outbound operation {operation!r} is unsupported")
        if request.get("final") or request.get("error"):
            await self._processing_finished(binding, bool(request.get("error")))
        return result


    async def _start_typing(self, interface, target_id):
        if interface in self.typing_tasks and not self.typing_tasks[interface].done():
            return

        async def loop():
            try:
                channel = await self._channel(target_id)
                while True:
                    await channel.trigger_typing()
                    await asyncio.sleep(self.config["outbound"]["typing"]["refresh_seconds"])
            except asyncio.CancelledError:
                pass

        self.typing_tasks[interface] = asyncio.create_task(loop())


    async def _stop_typing(self, interface):
        task = self.typing_tasks.pop(interface, None)
        if task:
            task.cancel()
            with suppress(Exception, asyncio.CancelledError):
                await task


    async def _processing_finished(self, binding, failed):
        await self._stop_typing(binding["interface"])
        source = self.turn_sources.pop(binding["interface"], None)
        if source is None or not self.config["outbound"]["reactions"]["enabled"] or not source.get("message_id"):
            return
        reactions = self.config["outbound"]["reactions"]
        with suppress(Exception):
            channel = await self._channel(source["target_id"])
            message = await channel.fetch_message(int(source["message_id"]))
            await message.remove_reaction(reactions["in_progress"], self.client.user)
            await message.add_reaction(reactions["failure"] if failed else reactions["success"])

    def _discord_embeds(self, request):
        return [self.discord.Embed.from_dict(value) for value in request.get("embeds", [])]

    def _discord_files(self, request):
        files = []
        for attachment in request.get("attachments", []):
            filename = attachment.get("filename") or os.path.basename(attachment["path"])
            if attachment.get("spoiler") and not filename.startswith("SPOILER_"):
                filename = "SPOILER_" + filename
            files.append(self.discord.File(attachment["path"], filename=filename, description=attachment.get("description")))
        return files

    def _discord_view(self, request):
        rows = request.get("components") or []
        if not rows:
            return None
        driver = self

        class Button(self.discord.ui.Button):
            async def callback(self, interaction):
                await driver._interaction(interaction)

        class Select(self.discord.ui.Select):
            async def callback(self, interaction):
                await driver._interaction(interaction)

        view = self.discord.ui.View(timeout=self.config["components"]["pending_ttl_seconds"])
        button_styles = {"primary": self.discord.ButtonStyle.primary, "secondary": self.discord.ButtonStyle.secondary, "success": self.discord.ButtonStyle.success, "danger": self.discord.ButtonStyle.danger, "link": self.discord.ButtonStyle.link}
        for row_index, row in enumerate(rows):
            for item in row:
                if item["type"] == "button":
                    component = Button(label=item.get("label"), style=button_styles.get(item.get("style", "secondary"), self.discord.ButtonStyle.secondary), custom_id=item.get("custom_id"), url=item.get("url"), emoji=item.get("emoji"), disabled=bool(item.get("disabled", False)), row=row_index)
                else:
                    options = [self.discord.SelectOption(label=option["label"], value=option["value"], description=option.get("description"), emoji=option.get("emoji"), default=bool(option.get("default", False))) for option in item["options"]]
                    component = Select(custom_id=item["custom_id"], placeholder=item.get("placeholder"), min_values=int(item.get("min_values", 1)), max_values=int(item.get("max_values", 1)), options=options, disabled=bool(item.get("disabled", False)), row=row_index)
                view.add_item(component)
        return view

    async def _reference(self, channel, message_id):
        if not message_id:
            return None
        try:
            message = await channel.fetch_message(int(message_id))
            return message.to_reference(fail_if_not_exists=False)
        except Exception:
            return None

    async def _message_create(self, binding, request):
        target_id = binding["thread_id"] or binding["channel_id"]
        channel = await self._channel(target_id)
        text = request.get("text", "")
        if not isinstance(text, str):
            raise ValueError("discord message.create text must be a string")
        if request.get("native_voice"):
            return await self._native_voice_create(channel, request)
        chunks = split_message(text, self.config["outbound"]["max_message_len"])
        embeds = self._discord_embeds(request)
        files = self._discord_files(request)
        view = self._discord_view(request)
        if not chunks and not embeds and not files:
            raise ValueError("discord message.create has no deliverable content")
        if getattr(channel, "type", None) == self.discord.ChannelType.forum:
            name = (request.get("thread_name") or text.split("\n", 1)[0] or "New Post")[:100]
            starter = chunks[0] if chunks else None
            created = await channel.create_thread(name=name, content=starter, embeds=embeds, files=files, view=view)
            thread = created.thread if hasattr(created, "thread") else created
            message = getattr(created, "message", None)
            message_ids = [str(message.id)] if message else []
            for chunk in chunks[1:]:
                sent = await thread.send(chunk)
                message_ids.append(str(sent.id))
            return {"operation": "message.create", "thread_id": str(thread.id), "message_ids": message_ids}
        reply_mode = self.config["outbound"]["reply_mode"]
        reference = None if reply_mode == "off" else await self._reference(channel, request.get("reply_to_message_id"))
        nonce = request["idempotency_key"]
        message_ids = []
        if not chunks:
            chunks = [None]
        for index, chunk in enumerate(chunks):
            sent = await channel.send(
                content=chunk,
                embeds=embeds if index == 0 else [],
                files=files if index == 0 else [],
                view=view if index == 0 else None,
                reference=reference if reply_mode == "all" or index == 0 else None,
                silent=bool(request.get("silent", False)),
                tts=bool(request.get("tts", False)),
                nonce=nonce if index == 0 else f"{nonce}-{index}",
            )
            message_ids.append(str(sent.id))
        if request.get("stream") and message_ids:
            update_stream_state(message_id=message_ids[0], last_edit_ms=int(time.time() * 1000))
        return {"operation": "message.create", "channel_id": str(channel.id), "message_ids": message_ids}

    async def _native_voice_create(self, channel, request):
        if getattr(channel, "type", None) == self.discord.ChannelType.forum:
            raise ValueError("discord native voice messages cannot create forum posts")
        attachment = request["attachments"][0]
        from discord_voice import decode_to_pcm
        pcm = await asyncio.to_thread(decode_to_pcm, attachment["path"], self.config["voice"]["max_decoded_pcm_bytes"])
        if not pcm:
            raise ValueError("discord native voice attachment is not decodable OGG Opus audio")
        duration, waveform = native_voice_metadata(pcm)
        discord = self.discord

        class NativeVoiceFile(discord.File):
            def to_dict(self, index):
                payload = super().to_dict(index)
                payload["duration_secs"] = duration
                payload["waveform"] = waveform
                return payload

        flags = discord.MessageFlags(voice=True)
        flags.suppress_notifications = bool(request.get("silent", False))
        file = NativeVoiceFile(attachment["path"], filename=attachment["filename"], description=attachment.get("description"))
        with discord.http.handle_message_parameters(file=file, nonce=request["idempotency_key"], flags=flags) as params:
            data = await self.client.http.send_message(channel.id, params=params)
        message = channel._state.create_message(channel=channel, data=data)
        return {"operation": "message.create", "channel_id": str(channel.id), "message_ids": [str(message.id)], "native_voice": True, "duration_seconds": duration}

    async def _message_edit(self, binding, request):
        message_id = require_snowflake(request.get("message_id"), "discord message.edit message_id")
        channel = await self._channel(binding["thread_id"] or binding["channel_id"])
        message = await channel.fetch_message(int(message_id))
        text = request.get("text")
        if text is not None and not isinstance(text, str):
            raise ValueError("discord message.edit text must be a string")
        kwargs = {}
        if "text" in request:
            kwargs["content"] = text
        if "embeds" in request:
            kwargs["embeds"] = self._discord_embeds(request)
        if "components" in request:
            kwargs["view"] = self._discord_view(request)
        if not kwargs:
            raise ValueError("discord message.edit has no editable fields")
        await message.edit(**kwargs)
        if request.get("stream"):
            update_stream_state(message_id=message_id, last_edit_ms=int(time.time() * 1000))
        if request.get("final"):
            update_stream_state(message_id=None, text="", last_seq=None, last_edit_ms=int(time.time() * 1000))
        return {"operation": "message.edit", "message_id": message_id}

    async def _message_delete(self, binding, request):
        message_id = require_snowflake(request.get("message_id"), "discord message.delete message_id")
        channel = await self._channel(binding["thread_id"] or binding["channel_id"])
        message = await channel.fetch_message(int(message_id))
        await message.delete()
        return {"operation": "message.delete", "message_id": message_id}

    async def _reaction(self, binding, request):
        message_id = require_snowflake(request.get("message_id"), "discord reaction message_id")
        emoji = request.get("emoji")
        if request["operation"] != "reaction.clear" or emoji is not None:
            emoji = require_string(emoji, "discord reaction emoji")
        channel = await self._channel(binding["thread_id"] or binding["channel_id"])
        message = await channel.fetch_message(int(message_id))
        if request["operation"] == "reaction.add":
            await message.add_reaction(emoji)
        elif request["operation"] == "reaction.remove":
            user_id = request.get("user_id")
            user = self.client.user if user_id is None or str(user_id) == str(self.client.user.id) else await self.client.fetch_user(int(require_snowflake(user_id, "discord reaction user_id")))
            await message.remove_reaction(emoji, user)
        else:
            if request.get("emoji"):
                await message.clear_reaction(emoji)
            else:
                await message.clear_reactions()
        return {"operation": request["operation"], "message_id": message_id, "emoji": emoji}

    async def _thread_create(self, binding, request):
        if binding["thread_id"] is not None:
            raise ValueError("discord thread.create cannot create a child of a thread binding")
        channel = await self._channel(binding["channel_id"])
        name = require_string(request.get("name"), "discord thread.create name")[:100]
        archive = int(request.get("auto_archive_duration", self.config["gating"]["auto_thread_archive_minutes"]))
        if archive not in (60, 1440, 4320, 10080):
            raise ValueError("discord thread.create auto_archive_duration is invalid")
        message_id = request.get("message_id")
        if message_id:
            message = await channel.fetch_message(int(require_snowflake(message_id, "discord thread.create message_id")))
            thread = await message.create_thread(name=name, auto_archive_duration=archive)
        else:
            thread = await channel.create_thread(name=name, auto_archive_duration=archive, type=self.discord.ChannelType.public_thread)
        mark_thread_participated(str(thread.id))
        return {"operation": "thread.create", "thread_id": str(thread.id), "name": thread.name}

    async def _thread_edit(self, binding, request):
        if binding["thread_id"] is None:
            raise ValueError("discord thread.edit requires a thread binding")
        thread = await self._channel(binding["thread_id"])
        kwargs = {}
        for key in ("name", "archived", "locked", "slowmode_delay", "auto_archive_duration"):
            if key in request:
                kwargs[key] = request[key]
        if not kwargs:
            raise ValueError("discord thread.edit has no editable fields")
        await thread.edit(**kwargs)
        return {"operation": "thread.edit", "thread_id": str(thread.id), "updated": sorted(kwargs)}

    async def _interaction_respond(self, request):
        interaction_id = require_snowflake(request.get("interaction_id"), "discord interaction.respond interaction_id")
        entry = self.pending_interactions.get(interaction_id)
        if entry is None or time.monotonic() - entry[0] > self.config["components"]["pending_ttl_seconds"]:
            raise RuntimeError("discord interaction is absent or expired")
        interaction = entry[1]
        text = request.get("text")
        if text is not None and not isinstance(text, str):
            raise ValueError("discord interaction.respond text must be a string")
        ephemeral = bool(request.get("ephemeral", False))
        embeds = self._discord_embeds(request)
        view = self._discord_view(request)
        if interaction.response.is_done():
            message = await interaction.followup.send(content=text, embeds=embeds, view=view, ephemeral=ephemeral, wait=True)
        else:
            await interaction.response.send_message(content=text, embeds=embeds, view=view, ephemeral=ephemeral)
            message = await interaction.original_response()
        if bool(request.get("final", True)):
            self.pending_interactions.pop(interaction_id, None)
        return {"operation": "interaction.respond", "interaction_id": interaction_id, "message_id": str(message.id)}

    def _load_opus(self):
        if self.discord.opus.is_loaded():
            return True
        import ctypes.util
        candidates = [ctypes.util.find_library("opus")]
        if sys.platform == "darwin":
            candidates.extend(("/opt/homebrew/lib/libopus.dylib", "/usr/local/lib/libopus.dylib"))
        for candidate in candidates:
            if not candidate:
                continue
            try:
                self.discord.opus.load_opus(candidate)
                if self.discord.opus.is_loaded():
                    return True
            except Exception:
                pass
        return False

    async def _voice_join(self, binding, request):
        if not self.config["voice"]["enabled"]:
            raise RuntimeError("discord voice is disabled by interface config")
        if not self._load_opus() and self.config["voice"]["require_opus"]:
            raise RuntimeError("discord voice requires a loadable libopus")
        voice_channel_id = require_snowflake(request.get("voice_channel_id"), "discord voice.join voice_channel_id")
        voice_channel = await self._channel(voice_channel_id)
        text_channel = await self._channel(binding["thread_id"] or binding["channel_id"])
        if not hasattr(voice_channel, "connect") or getattr(voice_channel, "guild", None) is None:
            raise ValueError("discord voice.join target is not a voice channel")
        if getattr(text_channel, "guild", None) is None or voice_channel.guild.id != text_channel.guild.id:
            raise ValueError("discord voice.join target is outside the bound guild")
        guild_id = voice_channel.guild.id
        existing = self.voice_clients.get(guild_id)
        if existing and existing.is_connected():
            if existing.channel.id != voice_channel.id:
                await existing.move_to(voice_channel)
            client = existing
        else:
            client = await voice_channel.connect(reconnect=True)
            self.voice_clients[guild_id] = client
        from discord_voice import VoiceMixer, VoiceReceiver, synth_ambient_pcm
        receiver = self.voice_receivers.get(guild_id)
        if receiver is None:
            def voice_user_allowed(user_id):
                user = voice_channel.guild.get_member(int(user_id))
                return user is not None and authorized(self.config, user, voice_channel.guild, text_channel)

            receiver = VoiceReceiver(
                client,
                self.config["voice"]["silence_seconds"],
                self.config["voice"]["minimum_speech_seconds"],
                self.config["voice"]["maximum_utterance_seconds"],
                self.config["voice"]["max_decoded_pcm_bytes"],
                self.config["voice"]["max_buffered_pcm_bytes"],
                self.config["voice"]["max_active_speakers"],
                voice_user_allowed,
            )
            receiver.start()
            self.voice_receivers[guild_id] = receiver
            self.voice_tasks[guild_id] = asyncio.create_task(self._voice_listen(guild_id))
        ambient = self.config["voice"]["ambient"]
        if guild_id not in self.voice_mixers and ambient["enabled"]:
            mixer = VoiceMixer(ambient["gain"], ambient["duck_gain"], ambient["speech_gain"], ambient["duck_release_ms"])
            pcm = None
            if ambient["blob_id"]:
                from discord_voice import decode_to_pcm
                pcm = await asyncio.to_thread(
                    decode_to_pcm,
                    safe_blob_path(ambient["blob_id"], self.config["media"]["max_upload_bytes"]),
                    self.config["voice"]["max_decoded_pcm_bytes"],
                )
            if pcm is None:
                pcm = await asyncio.to_thread(synth_ambient_pcm)
            mixer.set_ambient(pcm)
            if client.is_playing():
                client.stop()
            class MixerSource(self.discord.AudioSource):
                def read(self):
                    return mixer.read()

                def is_opus(self):
                    return False

                def cleanup(self):
                    mixer.cleanup()

            client.play(MixerSource())
            self.voice_mixers[guild_id] = mixer
        self.voice_bindings[guild_id] = binding["interface"]
        self._reset_voice_timeout(guild_id)
        return {"operation": "voice.join", "guild_id": str(guild_id), "voice_channel_id": str(voice_channel.id), "voice_channel_name": voice_channel.name}

    async def _voice_leave(self, binding, request):
        target = await self._channel(binding["thread_id"] or binding["channel_id"])
        guild = getattr(target, "guild", None)
        if guild is None:
            raise ValueError("discord voice.leave binding has no guild")
        guild_id = guild.id
        receiver = self.voice_receivers.pop(guild_id, None)
        if receiver:
            receiver.stop()
        task = self.voice_tasks.pop(guild_id, None)
        if task:
            task.cancel()
        timeout = self.voice_timeouts.pop(guild_id, None)
        if timeout:
            timeout.cancel()
        mixer = self.voice_mixers.pop(guild_id, None)
        if mixer:
            mixer.cleanup()
        client = self.voice_clients.pop(guild_id, None)
        if client and client.is_connected():
            if client.is_playing():
                client.stop()
            await client.disconnect(force=True)
        self.voice_bindings.pop(guild_id, None)
        return {"operation": "voice.leave", "guild_id": str(guild_id)}

    async def _voice_play(self, binding, request):
        target = await self._channel(binding["thread_id"] or binding["channel_id"])
        guild = getattr(target, "guild", None)
        if guild is None:
            raise ValueError("discord voice.play binding has no guild")
        client = self.voice_clients.get(guild.id)
        if client is None or not client.is_connected():
            raise RuntimeError("discord voice.play has no connected voice client")
        audio_path = request.get("audio_path")
        if not isinstance(audio_path, str) or not os.path.isfile(audio_path):
            raise ValueError("discord voice.play has no materialized audio blob")
        mixer = self.voice_mixers.get(guild.id)
        if mixer:
            from discord_voice import decode_to_pcm
            pcm = await asyncio.to_thread(decode_to_pcm, audio_path, self.config["voice"]["max_decoded_pcm_bytes"])
            if not pcm:
                raise RuntimeError("discord voice.play failed to decode audio")
            mixer.play_speech(pcm)
            deadline = time.monotonic() + self.config["voice"]["playback_timeout_seconds"]
            while mixer.speech_active:
                if time.monotonic() >= deadline:
                    mixer.stop_speech()
                    raise TimeoutError("discord voice.play timed out")
                await asyncio.sleep(0.05)
        else:
            receiver = self.voice_receivers.get(guild.id)
            if receiver:
                receiver.paused = True
            try:
                while client.is_playing():
                    await asyncio.sleep(0.05)
                done = asyncio.Event()
                event_loop = asyncio.get_running_loop()

                def after(_error):
                    event_loop.call_soon_threadsafe(done.set)

                client.play(self.discord.FFmpegPCMAudio(audio_path), after=after)
                await asyncio.wait_for(done.wait(), self.config["voice"]["playback_timeout_seconds"])
            finally:
                if receiver:
                    receiver.paused = False
        self._reset_voice_timeout(guild.id)
        return {"operation": "voice.play", "guild_id": str(guild.id), "bytes": os.path.getsize(audio_path)}

    def _reset_voice_timeout(self, guild_id):
        previous = self.voice_timeouts.pop(guild_id, None)
        if previous:
            previous.cancel()

        async def expire():
            try:
                await asyncio.sleep(self.config["voice"]["inactivity_seconds"])
                interface = self.voice_bindings.get(guild_id)
                binding = binding_by_interface(interface) if interface else None
                if binding:
                    await self._voice_leave(binding, {})
            except asyncio.CancelledError:
                pass

        self.voice_timeouts[guild_id] = asyncio.create_task(expire())

    async def _voice_listen(self, guild_id):
        from discord_voice import pcm_to_wav
        receiver = self.voice_receivers[guild_id]
        try:
            while receiver.running:
                await asyncio.sleep(0.2)
                for user_id, pcm in receiver.completed():
                    interface = self.voice_bindings.get(guild_id)
                    binding = binding_by_interface(interface) if interface else None
                    if binding is None:
                        continue
                    guild = self.client.get_guild(guild_id)
                    user = guild.get_member(user_id) if guild else None
                    channel = await self._channel(binding["thread_id"] or binding["channel_id"])
                    if user is None or not authorized(self.config, user, guild, channel):
                        continue
                    wav_path = await asyncio.to_thread(pcm_to_wav, pcm)
                    target = os.path.join(MEDIA_DIR, f"voice-{guild_id}-{user_id}-{time.time_ns()}.wav")
                    os.replace(wav_path, target)
                    event_id = f"voice:{guild_id}:{user_id}:{time.time_ns()}"
                    payload = {"source": "discord", "event_type": "voice.utterance", "event_id": event_id, "channel_id": binding["channel_id"], "thread_id": binding["thread_id"], "guild_id": str(guild_id), "author": jsonable_user(user), "audio": {"payload_path": target, "content_type": "audio/wav", "size": os.path.getsize(target), "sample_rate": 16000, "channels": 1}}
                    payload = await asyncio.to_thread(grant_payload_media, binding, payload)
                    await asyncio.to_thread(self.hub.dispatch, binding, payload)
                    is_duplicate_event(event_id)
                    self._reset_voice_timeout(guild_id)
        except asyncio.CancelledError:
            pass


def load_bindings_safe():
    from discord_boundary import load_bindings
    return list(load_bindings()["bindings"].values())


BINDING_EVENTS_PATH = os.path.join(VAR_DIR, "binding-events.json")
BINDING_EVENTS_LOCK = threading.RLock()


def process_arrival(config, link, payload):
    payload = require_object(payload, "discord binding arrival")
    binding = config["binding"]
    if payload.get("channel_id") != binding["channel_id"] or payload.get("thread_id") != binding["thread_id"]:
        raise ValueError("discord gateway delivered coordinates outside the private binding")
    event_id = require_string(payload.get("event_id"), "discord binding event_id")
    payload = materialize_granted_media(config, link, payload)
    payload_sha256 = hashlib.sha256(json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")).hexdigest()
    with BINDING_EVENTS_LOCK:
        state = read_json(BINDING_EVENTS_PATH, {"schema_version": 2, "events": {}})
        state = require_keys(state, {"schema_version", "events"}, set(), "discord binding events")
        if state["schema_version"] != 2 or not isinstance(state["events"], dict):
            raise ValueError("discord binding events state is malformed")
        for known_event_id, record in state["events"].items():
            require_string(known_event_id, "discord binding event id")
            record = require_keys(record, {"payload_sha256", "admitted_seq"}, set(), f"discord binding event {known_event_id}")
            if not re.fullmatch(r"[0-9a-f]{64}", require_string(record["payload_sha256"], f"discord binding event {known_event_id}.payload_sha256")):
                raise ValueError(f"discord binding event {known_event_id}.payload_sha256 is malformed")
            require_int(record["admitted_seq"], f"discord binding event {known_event_id}.admitted_seq", 0)
        existing = state["events"].get(event_id)
        if existing is not None:
            if existing["payload_sha256"] != payload_sha256:
                raise RuntimeError(f"discord event {event_id!r} was reused with different payload bytes")
            return existing["admitted_seq"]
        admitted = hand_json_idempotent("discord.inbound", payload, f"discord-in-{hashlib.sha256(event_id.encode('utf-8')).hexdigest()[:32]}")
        state["events"][event_id] = {"payload_sha256": payload_sha256, "admitted_seq": admitted}
        limit = config["inbound_journal"]["private_admitted_limit"]
        if len(state["events"]) > limit:
            state["events"] = dict(list(state["events"].items())[-limit:])
        atomic_write_json(BINDING_EVENTS_PATH, state)
        return admitted


def process_outbound(config, link, source_seq, content, run):
    binding = config["binding"]
    if content is None or run is None:
        hand_failure(source_seq, "poisoned_outbound", {"error": "movement has no artifact content or runtime run"}, f"discord-outbound-poison-{source_seq}")
        return
    if run != binding["run"]:
        hand_failure(source_seq, "boundary_rejected", {"error": "movement run does not match private interface", "expected_run": binding["run"], "observed_run": run}, f"discord-outbound-run-{source_seq}")
        return
    content_hash = content_hexdigest(content)
    effect = load_effect(source_seq)
    if effect is not None and (effect["run"] != run or effect["source_artifact_hash"] != content_hash):
        hand_failure(source_seq, "effect_journal_mismatch", {"source_artifact_hash": content_hash}, f"discord-effect-mismatch-{source_seq}")
        return
    if effect is not None and effect["status"] in ("evidenced", "failure_evidenced"):
        return
    if effect is not None and effect["status"] == "succeeded":
        admitted = hand_delivery_result(source_seq, content_hash, run, effect["result"])
        effect["status"] = "evidenced"
        effect["evidence_movement_seq"] = admitted
        commit_effect(source_seq, effect)
        return
    if effect is not None and effect["status"] in ("started", "indeterminate"):
        admitted = hand_failure(source_seq, "indeterminate_external_effect", {"source_artifact_hash": content_hash, "error": effect.get("error")}, f"discord-effect-indeterminate-{source_seq}")
        effect["status"] = "failure_evidenced"
        effect["evidence_movement_seq"] = admitted
        commit_effect(source_seq, effect)
        return
    try:
        label, payload = materialize_payload(
            content,
            config["driver_ipc"]["max_manifest_bytes"],
            config["driver_ipc"]["max_frame_bytes"],
        )
        request, result, state_updates = decode_output(config, label, payload, binding, source_seq)
    except Exception as error:
        hand_failure(source_seq, "boundary_rejected", {"source_artifact_hash": content_hash, "error": sanitize_external_error(config, error)}, f"discord-outbound-boundary-{source_seq}")
        return
    effect = {"schema_version": 1, "source_movement_seq": source_seq, "source_artifact_hash": content_hash, "run": run, "artifact_label": label, "status": "started" if request is not None else "succeeded"}
    if request is None:
        if state_updates:
            update_stream_state(**state_updates)
        effect["result"] = result
        commit_effect(source_seq, effect)
        admitted = hand_delivery_result(source_seq, content_hash, run, result)
        effect["status"] = "evidenced"
        effect["evidence_movement_seq"] = admitted
        commit_effect(source_seq, effect)
        return
    commit_effect(source_seq, effect)
    try:
        delivery = link.request("outbound", {"interface": INTERFACE_ID, "request": request})
        if not isinstance(delivery, dict):
            raise RuntimeError("discord gateway returned a non-object delivery result")
        if request["operation"] == "message.create" and request.get("stream"):
            message_ids = delivery.get("message_ids") or []
            if message_ids:
                state_updates["message_id"] = message_ids[0]
        if request.get("stream"):
            state_updates["last_edit_ms"] = int(time.time() * 1000)
        if request.get("final"):
            state_updates["message_id"] = None
        if state_updates:
            update_stream_state(**state_updates)
        result = {**result, "delivery": delivery}
    except Exception as error:
        message = sanitize_external_error(config, error)
        effect["status"] = "indeterminate"
        effect["error"] = message
        commit_effect(source_seq, effect)
        admitted = hand_failure(source_seq, "indeterminate_external_effect", {"source_artifact_hash": content_hash, "error": message}, f"discord-effect-indeterminate-{source_seq}")
        effect["status"] = "failure_evidenced"
        effect["evidence_movement_seq"] = admitted
        commit_effect(source_seq, effect)
        return
    effect["status"] = "succeeded"
    effect["result"] = result
    commit_effect(source_seq, effect)
    admitted = hand_delivery_result(source_seq, content_hash, run, result)
    effect["status"] = "evidenced"
    effect["evidence_movement_seq"] = admitted
    commit_effect(source_seq, effect)


def run_binding(config):
    link = BindingLink(config, lambda payload, current_link: process_arrival(config, current_link, payload))
    next_seq = load_cursor()
    binding = config["binding"]
    sys.stderr.write(f"discord binding up: interface={INTERFACE_ID} run={binding['run']} ledger_next_seq={next_seq}\n")
    sys.stderr.flush()
    signal_ready()
    while True:
        for source_seq, content, run in read_own_outbound(next_seq, max_row_bytes=config["ledger"]["max_row_bytes"]):
            process_outbound(config, link, source_seq, content, run)
            next_seq = source_seq + 1
            commit_cursor(next_seq)
        time.sleep(0.05)


def run_gateway(config, token):
    try:
        driver = GatewayDriver(config, token)
        signal_ready()
        asyncio.run(driver.run())
    except KeyboardInterrupt:
        raise
    except Exception as error:
        path = os.path.join(VAR_DIR, "gateway-reconnect.json")
        state = read_json(path, {"schema_version": 1, "backoff_ms": config["discord_api"]["reconnect_backoff_ms"]})
        state = require_keys(state, {"schema_version", "backoff_ms"}, set(), "discord gateway reconnect state")
        if state["schema_version"] != 1:
            raise RuntimeError("discord gateway reconnect state schema_version is invalid") from error
        backoff_ms = require_int(state["backoff_ms"], "discord gateway reconnect backoff_ms", 1)
        next_backoff = min(backoff_ms * 2, config["discord_api"]["reconnect_backoff_cap_ms"])
        atomic_write_json(path, {"schema_version": 1, "backoff_ms": next_backoff})
        backoff = backoff_ms / 1000.0
        sys.stderr.write(f"discord gateway exited: {json.dumps(external_error_observation(error), sort_keys=True)}; restarting in {backoff:.3f}s\n")
        sys.stderr.flush()
        time.sleep(backoff)
        os.execv(sys.executable, [sys.executable] + sys.argv)


def main():
    config = load_config()
    if "--validate-config" in sys.argv:
        print(json.dumps({"ok": True, "mode": config["mode"], "interface": INTERFACE_ID}))
        return
    if "--pairing-approve" in sys.argv:
        index = sys.argv.index("--pairing-approve")
        if index + 1 >= len(sys.argv):
            raise ValueError("--pairing-approve requires a code")
        print(json.dumps(approve_pairing(sys.argv[index + 1]), sort_keys=True))
        return
    if "--pairing-revoke" in sys.argv:
        index = sys.argv.index("--pairing-revoke")
        if index + 1 >= len(sys.argv):
            raise ValueError("--pairing-revoke requires a user id")
        print(json.dumps(revoke_pairing(sys.argv[index + 1]), sort_keys=True))
        return
    start_runtime_verdict_reader()
    if config["mode"] == "binding":
        run_binding(config)
    else:
        run_gateway(config, resolve_token(config))


if __name__ == "__main__":
    main()
