import hashlib
import os
import stat


class CryptoFailure(RuntimeError):
    def __init__(self, category, message, details=None):
        super().__init__(message)
        self.category = category
        self.details = details or {}


def external_crypto_failure(category, error, details=None):
    body = str(error).encode("utf-8", "replace")
    evidence = dict(details or {})
    evidence.update(
        {
            "exception": type(error).__name__,
            "error_bytes": len(body),
            "error_sha256": hashlib.sha256(body).hexdigest(),
        }
    )
    return CryptoFailure(category, "Matrix crypto operation failed", evidence)


class CryptoStateStore:
    def __init__(self, state_store, joined_rooms):
        self.state_store = state_store
        self.joined_rooms = joined_rooms

    async def is_encrypted(self, room_id):
        return (await self.get_encryption_info(room_id)) is not None

    async def get_encryption_info(self, room_id):
        method = getattr(self.state_store, "get_encryption_info", None)
        if method is None:
            return None
        return await method(room_id)

    async def find_shared_rooms(self, user_id):
        return list(self.joined_rooms)


def dependency_status():
    missing = []
    for module in ("mautrix.crypto", "mautrix.crypto.store.asyncpg", "aiosqlite", "asyncpg"):
        try:
            __import__(module)
        except (ImportError, AttributeError):
            missing.append(module)
    return missing


def normalize_mode(value):
    if not isinstance(value, str) or not value or value.strip() != value:
        raise CryptoFailure("invalid_crypto_mode", f"unsupported Matrix E2EE mode {value!r}")
    value = value.lower()
    if value in {"required", "require", "true", "1", "yes", "on"}:
        return "required"
    if value in {"optional", "prefer", "preferred"}:
        return "optional"
    if value in {"off", "false", "0", "no"}:
        return "off"
    raise CryptoFailure("invalid_crypto_mode", f"unsupported Matrix E2EE mode {value!r}")


def extract_server_ed25519(device_keys):
    for key_id, value in (getattr(device_keys, "keys", {}) or {}).items():
        if str(key_id).startswith("ed25519:"):
            return str(value)
    return None


class MatrixCrypto:
    def __init__(self, config, var_dir, joined_rooms):
        self.config = config
        self.var_dir = var_dir
        configured = os.environ.get(config["mode_env"], config["mode"])
        self.mode = normalize_mode(configured)
        self.joined_rooms = joined_rooms
        self.database = None
        self.store = None
        self.machine = None
        self.client = None
        self.bootstrap_state = "off" if self.mode == "off" else "not_started"
        self.generated_key_path = None

    @property
    def enabled(self):
        return self.machine is not None

    def store_dir(self):
        path = os.path.join(self.var_dir, self.config["store_subdir"])
        os.makedirs(path, mode=0o700, exist_ok=True)
        current = os.lstat(path)
        if stat.S_ISLNK(current.st_mode) or not stat.S_ISDIR(current.st_mode) or current.st_uid != os.getuid():
            raise CryptoFailure("crypto_store_permissions", "Matrix crypto store must be an owner-controlled directory")
        if stat.S_IMODE(current.st_mode) != 0o700:
            os.chmod(path, 0o700)
        return path

    def pickle_key(self):
        path = os.path.join(self.store_dir(), "pickle-key")
        try:
            descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        except FileNotFoundError:
            value = os.urandom(32).hex()
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            try:
                with os.fdopen(descriptor, "w", encoding="ascii") as handle:
                    descriptor = None
                    handle.write(value)
                    handle.write("\n")
                    handle.flush()
                    os.fsync(handle.fileno())
            finally:
                if descriptor is not None:
                    os.close(descriptor)
            os.chmod(path, 0o600)
            directory = os.open(self.store_dir(), os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
            return value
        try:
            metadata = os.fstat(descriptor)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o600:
                raise CryptoFailure("pickle_key_permissions", "Matrix crypto pickle key must be a regular mode-0600 file")
            with os.fdopen(descriptor, "r", encoding="ascii") as handle:
                descriptor = None
                value = handle.read(128).strip()
        finally:
            if descriptor is not None:
                os.close(descriptor)
        if len(value) != 64 or any(character not in "0123456789abcdef" for character in value):
            raise CryptoFailure("pickle_key_malformed", "Matrix crypto pickle key is malformed")
        return value

    async def start(self, client, state_store, user_id, device_id):
        self.client = client
        if self.mode == "off":
            return False
        if not device_id:
            if self.mode == "optional":
                self.bootstrap_state = "optional_device_id_missing"
                return False
            raise CryptoFailure("device_id_missing", "Matrix E2EE requires a concrete device id")
        missing = dependency_status()
        if missing:
            if self.mode == "optional":
                self.bootstrap_state = "optional_dependencies_missing"
                return False
            raise CryptoFailure("crypto_dependencies_missing", "Matrix E2EE dependencies are missing", {"modules": missing})
        try:
            from mautrix.crypto import OlmMachine
            from mautrix.crypto.store.asyncpg import PgCryptoStore
            from mautrix.types import TrustState
            from mautrix.util.async_db import Database

            path = os.path.join(self.store_dir(), "crypto.db")
            if os.path.lexists(path):
                current = os.lstat(path)
                if stat.S_ISLNK(current.st_mode) or not stat.S_ISREG(current.st_mode) or current.st_uid != os.getuid() or stat.S_IMODE(current.st_mode) != 0o600:
                    raise CryptoFailure("crypto_database_permissions", "Matrix crypto database must be an owner-controlled mode-0600 file")
            self.database = Database.create(f"sqlite:///{path}", upgrade_table=PgCryptoStore.upgrade_table)
            await self.database.start()
            self.store = PgCryptoStore(account_id=user_id, pickle_key=self.pickle_key(), db=self.database)
            await self.store.open()
            if device_id:
                await self.store.put_device_id(device_id)
            machine = OlmMachine(client, self.store, CryptoStateStore(state_store, self.joined_rooms))
            minimum = TrustState.VERIFIED if self.config["minimum_device_trust"] == "verified" else TrustState.UNVERIFIED
            machine.share_keys_min_trust = minimum
            machine.send_keys_min_trust = minimum
            await machine.load()
            if not await self.verify_device_keys(client, machine):
                raise CryptoFailure("device_identity_mismatch", "Matrix device identity keys do not match the homeserver")
            try:
                await machine.share_keys()
            except Exception as error:
                if "already exists" in str(error).lower():
                    raise CryptoFailure("stale_one_time_keys", "Matrix device has stale one-time keys", {"device_id": str(device_id)}) from error
                raise
            self.machine = machine
            client.crypto = machine
            await self.bootstrap_cross_signing()
            return True
        except CryptoFailure:
            await self.close()
            if self.mode == "optional":
                self.bootstrap_state = "optional_failed"
                return False
            raise
        except Exception as error:
            await self.close()
            if self.mode == "optional":
                self.bootstrap_state = "optional_failed"
                return False
            raise external_crypto_failure("crypto_initialization_failed", error) from error

    async def verify_device_keys(self, client, machine):
        try:
            response = await client.query_keys({client.mxid: [client.device_id]})
        except Exception as error:
            raise external_crypto_failure("device_key_query_failed", error) from error
        devices = getattr(response, "device_keys", {}) or {}
        user_devices = devices.get(str(client.mxid)) or devices.get(client.mxid) or {}
        server_keys = user_devices.get(str(client.device_id)) or user_devices.get(client.device_id)
        local_key = machine.account.identity_keys.get("ed25519")
        if not server_keys:
            machine.account.shared = False
            try:
                await machine.share_keys()
            except Exception as error:
                raise external_crypto_failure("device_key_upload_failed", error) from error
            return await self.reverify_device_keys(client, machine, local_key)
        if extract_server_ed25519(server_keys) != local_key:
            return False
        return True

    async def reverify_device_keys(self, client, machine, local_key):
        try:
            response = await client.query_keys({client.mxid: [client.device_id]})
            devices = getattr(response, "device_keys", {}) or {}
            user_devices = devices.get(str(client.mxid)) or devices.get(client.mxid) or {}
            server_keys = user_devices.get(str(client.device_id)) or user_devices.get(client.device_id)
            return bool(server_keys and extract_server_ed25519(server_keys) == local_key)
        except Exception as error:
            raise external_crypto_failure("device_key_reverification_failed", error) from error

    def recovery_output_path(self):
        return os.path.join(self.store_dir(), self.config["generated_recovery_key_file"])

    def write_recovery_key(self, value):
        path = self.recovery_output_path()
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                descriptor = None
                handle.write(value)
                handle.write("\n")
                handle.flush()
                os.fsync(handle.fileno())
        finally:
            if descriptor is not None:
                os.close(descriptor)
        if stat.S_IMODE(os.stat(path).st_mode) != 0o600:
            raise CryptoFailure("recovery_key_permissions", "generated Matrix recovery key file is not mode 0600")
        self.generated_key_path = path
        return path

    async def bootstrap_cross_signing(self):
        recovery_key = os.environ.get(self.config["recovery_key_env"], "").strip()
        if recovery_key:
            try:
                await self.machine.verify_with_recovery_key(recovery_key)
                self.bootstrap_state = "verified_with_recovery_key"
                return
            except Exception as error:
                if self.config["require_cross_signing"]:
                    raise external_crypto_failure("recovery_key_verification_failed", error) from error
                self.bootstrap_state = "recovery_key_verification_failed"
                return
        try:
            existing = await self.machine.get_own_cross_signing_public_keys()
        except Exception as error:
            if self.config["require_cross_signing"]:
                raise external_crypto_failure("cross_signing_query_failed", error) from error
            self.bootstrap_state = "cross_signing_query_failed"
            return
        if existing is not None:
            self.bootstrap_state = "cross_signing_present"
            return
        output_path = self.recovery_output_path()
        if os.path.exists(output_path):
            if self.config["require_cross_signing"]:
                raise CryptoFailure("recovery_key_output_exists", "refusing to overwrite generated Matrix recovery key")
            self.bootstrap_state = "bootstrap_refused_existing_output"
            return
        try:
            generated = await self.machine.generate_recovery_key()
            self.write_recovery_key(generated)
            keys = await self.machine.get_own_cross_signing_public_keys()
            if keys is None:
                raise CryptoFailure("cross_signing_publication_failed", "Matrix cross-signing keys are absent after bootstrap")
            self.bootstrap_state = "cross_signing_bootstrapped"
        except CryptoFailure:
            raise
        except Exception as error:
            if self.config["require_cross_signing"]:
                raise external_crypto_failure("cross_signing_bootstrap_failed", error) from error
            self.bootstrap_state = "cross_signing_bootstrap_failed"

    async def room_encrypted(self, room_id):
        if not self.client:
            return False
        state_store = getattr(self.client, "state_store", None)
        if state_store is not None:
            method = getattr(state_store, "is_encrypted", None)
            if method is not None:
                value = await method(room_id)
                if value is not None:
                    return bool(value)
        try:
            from mautrix.errors import MNotFound
            from mautrix.types import EventType, RoomID

            await self.client.get_state_event(RoomID(room_id), EventType.ROOM_ENCRYPTION)
            return True
        except MNotFound:
            return False
        except Exception as error:
            raise external_crypto_failure("room_encryption_query_failed", error, {"room_id": str(room_id)}) from error

    async def encrypted_media(self, room_id, data):
        if not self.config["encrypt_media"] or not await self.room_encrypted(room_id):
            return data, None
        if not self.enabled:
            raise CryptoFailure("encrypted_room_unavailable", "cannot upload media to an encrypted Matrix room without crypto")
        try:
            from mautrix.crypto.attachments import encrypt_attachment

            encrypted, descriptor = encrypt_attachment(data)
            return encrypted, descriptor.serialize()
        except Exception as error:
            raise external_crypto_failure("attachment_encryption_failed", error) from error

    async def decrypt_media(self, descriptor, data):
        if not isinstance(descriptor, dict):
            return data
        key = descriptor.get("key") if isinstance(descriptor.get("key"), dict) else {}
        hashes = descriptor.get("hashes") if isinstance(descriptor.get("hashes"), dict) else {}
        if not isinstance(key.get("k"), str) or not isinstance(hashes.get("sha256"), str) or not isinstance(descriptor.get("iv"), str):
            raise CryptoFailure("encrypted_attachment_metadata_missing", "encrypted Matrix attachment is missing key, hash, or IV")
        try:
            from mautrix.crypto.attachments import decrypt_attachment

            return decrypt_attachment(data, key["k"], hashes["sha256"], descriptor["iv"])
        except Exception as error:
            raise external_crypto_failure("attachment_decryption_failed", error) from error

    async def query_devices(self, user_id):
        if not self.enabled:
            raise CryptoFailure("crypto_not_enabled", "Matrix device keys require E2EE")
        try:
            from mautrix.types import UserID

            await self.machine._fetch_keys([UserID(user_id)], include_untracked=True)
            devices = await self.store.get_devices(UserID(user_id)) or {}
            result = []
            for device_id, device in sorted(devices.items(), key=lambda item: str(item[0])):
                resolved = await self.machine.resolve_trust(device)
                result.append(
                    {
                        "device_id": str(device_id),
                        "name": str(device.name or ""),
                        "identity_key": str(device.identity_key),
                        "signing_key": str(device.signing_key),
                        "local_trust": str(getattr(device.trust, "name", device.trust)),
                        "resolved_trust": str(getattr(resolved, "name", resolved)),
                        "deleted": bool(device.deleted),
                    }
                )
            return result
        except CryptoFailure:
            raise
        except Exception as error:
            raise external_crypto_failure("device_query_failed", error, {"user_id": user_id}) from error

    async def verify_device(self, user_id, device_id, trust):
        if not self.enabled:
            raise CryptoFailure("crypto_not_enabled", "Matrix device trust requires E2EE")
        try:
            from mautrix.types import DeviceID, TrustState, UserID

            user = UserID(user_id)
            await self.machine._fetch_keys([user], include_untracked=True)
            devices = await self.store.get_devices(user) or {}
            device = devices.get(DeviceID(device_id)) or devices.get(device_id)
            if device is None:
                raise CryptoFailure("unknown_device", "Matrix device is not known", {"user_id": user_id, "device_id": device_id})
            states = {"verified": TrustState.VERIFIED, "unverified": TrustState.UNVERIFIED, "blacklisted": TrustState.BLACKLISTED}
            if trust not in states:
                raise CryptoFailure("invalid_device_trust", f"unsupported Matrix device trust {trust!r}")
            device.trust = states[trust]
            devices[DeviceID(device_id)] = device
            await self.store.put_devices(user, devices)
            resolved = await self.machine.resolve_trust(device, allow_fetch=False)
            return {"user_id": user_id, "device_id": device_id, "local_trust": trust, "resolved_trust": str(getattr(resolved, "name", resolved))}
        except CryptoFailure:
            raise
        except Exception as error:
            raise external_crypto_failure("device_trust_write_failed", error, {"user_id": user_id, "device_id": device_id}) from error

    def diagnostics(self):
        return {
            "mode": self.mode,
            "enabled": self.enabled,
            "dependencies_missing": dependency_status(),
            "bootstrap_state": self.bootstrap_state,
            "generated_recovery_key_present": os.path.exists(self.recovery_output_path()) if self.mode != "off" else False,
            "minimum_device_trust": self.config["minimum_device_trust"],
        }

    async def close(self):
        if self.client is not None and getattr(self.client, "crypto", None) is self.machine:
            self.client.crypto = None
        self.machine = None
        self.store = None
        if self.database is not None:
            database = self.database
            self.database = None
            try:
                await database.stop()
            except Exception:
                pass
        self.client = None
