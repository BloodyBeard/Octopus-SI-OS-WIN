import asyncio
import base64
import hashlib
import inspect
import json
import os
import re
import stat
import tempfile
import time
import urllib.parse

from matrix_contract import MXID_RE
from matrix_contract import ROOM_ID_RE
from matrix_contract import is_loopback_hostname
from matrix_contract import require_bool
from matrix_contract import require_event
from matrix_contract import require_int
from matrix_contract import require_keys
from matrix_contract import require_mxid
from matrix_contract import require_object
from matrix_contract import require_room
from matrix_contract import require_string
from matrix_contract import require_string_list
from matrix_contract import transaction_id
from matrix_contract import validate_proxy_url
from matrix_crypto import CryptoFailure
from matrix_crypto import MatrixCrypto
from matrix_rendering import apply_relation
from matrix_rendering import build_text_content
from matrix_rendering import split_text


class MatrixWireFailure(RuntimeError):
    def __init__(self, category, message, status=None, errcode=None, retry_after_ms=None, details=None):
        super().__init__(message)
        self.category = category
        self.status = status
        self.errcode = errcode
        self.retry_after_ms = retry_after_ms
        self.details = details or {}

    def evidence(self):
        return {
            "category": self.category,
            "message": str(self),
            "status": self.status,
            "errcode": self.errcode,
            "retry_after_ms": self.retry_after_ms,
            "details": self.details,
        }


def body_observation(body):
    return {
        "body_bytes": len(body),
        "body_sha256": hashlib.sha256(body).hexdigest(),
    }


def external_error_observation(error):
    body = str(error).encode("utf-8", "replace")
    return {"exception": type(error).__name__, **body_observation(body)}


def matrix_errcode(value):
    if isinstance(value, str) and re.fullmatch(r"M_[A-Z0-9_]{1,128}", value):
        return value
    return None


def json_value(value):
    if hasattr(value, "serialize"):
        value = value.serialize()
    if isinstance(value, dict):
        return {str(key): json_value(item) for key, item in value.items()}
    if isinstance(value, (list, tuple, set)):
        return [json_value(item) for item in value]
    if isinstance(value, (str, int, float, bool)) or value is None:
        return value
    if hasattr(value, "value"):
        return json_value(value.value)
    return str(value)


def event_type_name(value):
    if value is None:
        return ""
    if hasattr(value, "serialize"):
        return str(value.serialize())
    value = getattr(value, "value", value)
    return str(value)


def serialize_event(event):
    if isinstance(event, dict):
        result = json_value(event)
    elif hasattr(event, "serialize"):
        result = json_value(event.serialize())
    else:
        result = {}
    if not isinstance(result, dict):
        result = {}
    for key in ("room_id", "event_id", "sender", "state_key", "redacts", "origin_server_ts"):
        value = getattr(event, key, None)
        if value is not None and key not in result:
            result[key] = json_value(value)
    kind = getattr(event, "type", None)
    if kind is not None:
        result["type"] = event_type_name(kind)
    content = getattr(event, "content", None)
    if content is not None:
        result["content"] = json_value(content)
    unsigned = getattr(event, "unsigned", None)
    if unsigned is not None:
        result["unsigned"] = json_value(unsigned)
    trust_state = getattr(event, "trust_state", None)
    if trust_state is not None:
        result["decryption"] = {
            "decrypted": True,
            "trust_state": str(getattr(trust_state, "name", trust_state)),
            "forwarded_keys": bool(getattr(event, "forwarded_keys", False)),
        }
    return result


class MatrixWire:
    SUPPORTED_OPERATIONS = {
        "account.data.get",
        "device.keys.query",
        "device.verify",
        "location.send",
        "media.send",
        "message.edit",
        "message.redact",
        "message.send",
        "presence.set",
        "reaction.redact",
        "reaction.send",
        "receipt.read",
        "room.ban",
        "room.create",
        "room.history",
        "room.invite",
        "room.join",
        "room.kick",
        "room.leave",
        "room.members",
        "room.name.set",
        "room.state.get",
        "room.topic.set",
        "room.unban",
        "typing.clear",
        "typing.set",
    }

    def __init__(self, config, var_dir, on_event):
        declared = set(config["outbound"]["allowed_operations"])
        if declared != self.SUPPORTED_OPERATIONS:
            missing = sorted(declared - self.SUPPORTED_OPERATIONS)
            undeclared = sorted(self.SUPPORTED_OPERATIONS - declared)
            raise ValueError(f"Matrix operation catalog drifted: unimplemented={missing}, undeclared={undeclared}")
        self.config = config
        self.var_dir = var_dir
        self.on_event = on_event
        self.homeserver = ""
        self.access_token = ""
        self.user_id = ""
        self.device_id = ""
        self.client = None
        self.api = None
        self.crypto = MatrixCrypto(config["crypto"], var_dir, set())
        self.joined_rooms = self.crypto.joined_rooms
        self.dm_rooms = set()
        self.last_dm_refresh = 0.0
        self.room_cache = {}
        self.room_cache_time = {}
        self.last_sync_unix_ms = None
        self.connected_unix_ms = None
        self.session = None

    def resolve_credentials(self):
        transport = self.config["transport"]
        homeserver = os.environ.get(transport["homeserver_env"], "")
        token = os.environ.get(transport["access_token_env"], "")
        user_id = os.environ.get(transport["user_id_env"], "")
        password = os.environ.get(transport["password_env"], "")
        device_id = os.environ.get(transport["device_id_env"], "")
        if any(value.strip() != value for value in (homeserver, token, user_id, device_id)):
            raise MatrixWireFailure("credentials_invalid", "Matrix credential fields must not contain surrounding whitespace")
        homeserver = homeserver.rstrip("/")
        if not homeserver:
            raise MatrixWireFailure("credentials_missing", "Matrix homeserver is not configured")
        parsed = urllib.parse.urlsplit(homeserver)
        if parsed.scheme not in {"http", "https"} or not parsed.hostname or parsed.username or parsed.password:
            raise MatrixWireFailure("credentials_invalid", "Matrix homeserver URL is invalid")
        if parsed.scheme == "http" and not is_loopback_hostname(parsed.hostname):
            raise MatrixWireFailure("credentials_invalid", "Matrix homeserver must use HTTPS outside loopback")
        if not token and not (user_id and password):
            raise MatrixWireFailure("credentials_missing", "Matrix access token or user id plus password is required")
        if user_id and not MXID_RE.fullmatch(user_id):
            raise MatrixWireFailure("credentials_invalid", "Matrix user id is malformed")
        self.homeserver = homeserver
        self.access_token = token
        self.user_id = user_id
        self.device_id = device_id
        return password

    def create_session(self):
        import aiohttp

        try:
            proxy = validate_proxy_url(os.environ.get(self.config["transport"]["proxy_env"], ""))
        except ValueError as error:
            raise MatrixWireFailure("proxy_invalid", str(error)) from error
        timeout = aiohttp.ClientTimeout(total=self.config["transport"]["request_timeout_ms"] / 1000.0)
        headers = {"User-Agent": self.config["transport"]["user_agent"]}
        if proxy.lower().startswith(("socks://", "socks5://", "socks5h://", "socks4://")):
            try:
                from aiohttp_socks import ProxyConnector
            except ImportError as error:
                raise MatrixWireFailure("proxy_dependency_missing", "aiohttp-socks is required for Matrix SOCKS proxying") from error
            return aiohttp.ClientSession(connector=ProxyConnector.from_url(proxy, rdns=True), trust_env=False, timeout=timeout, headers=headers)
        if proxy:
            try:
                return aiohttp.ClientSession(proxy=proxy, trust_env=False, timeout=timeout, headers=headers)
            except TypeError as error:
                raise MatrixWireFailure("proxy_unsupported", "installed aiohttp does not support a session-wide Matrix proxy") from error
        return aiohttp.ClientSession(trust_env=False, timeout=timeout, headers=headers)

    async def connect(self):
        password = self.resolve_credentials()
        try:
            from mautrix.api import HTTPAPI
            from mautrix.client import Client
            from mautrix.client.state_store import MemoryStateStore, MemorySyncStore
            from mautrix.types import UserID
        except ImportError as error:
            raise MatrixWireFailure("dependency_missing", "mautrix is required by world_matrix") from error
        self.session = self.create_session()
        self.api = HTTPAPI(base_url=self.homeserver, token=self.access_token, client_session=self.session)
        state_store = MemoryStateStore()
        client = Client(
            mxid=UserID(self.user_id) if self.user_id else UserID(""),
            device_id=self.device_id or None,
            api=self.api,
            state_store=state_store,
            sync_store=MemorySyncStore(),
        )
        self.client = client
        if self.access_token:
            self.api.token = self.access_token
            try:
                identity = await client.whoami()
            except Exception as error:
                await self.close()
                raise self.translate_error("authentication_failed", error)
            self.user_id = str(getattr(identity, "user_id", "") or self.user_id)
            resolved_device = str(getattr(identity, "device_id", "") or "")
            self.device_id = self.device_id or resolved_device
            client.mxid = UserID(self.user_id)
            if self.device_id:
                client.device_id = self.device_id
        else:
            try:
                response = await client.login(
                    identifier=self.user_id,
                    password=password,
                    device_name="Octopus Matrix",
                    device_id=self.device_id or None,
                )
            except Exception as error:
                await self.close()
                raise self.translate_error("authentication_failed", error)
            self.access_token = str(getattr(response, "access_token", "") or self.api.token or "")
            self.device_id = str(getattr(response, "device_id", "") or self.device_id)
            self.user_id = str(getattr(response, "user_id", "") or self.user_id)
            if not self.access_token or not self.user_id:
                await self.close()
                raise MatrixWireFailure("authentication_failed", "Matrix password login returned incomplete credentials")
        await self.crypto.start(client, state_store, self.user_id, self.device_id)
        self.register_handlers()
        self.connected_unix_ms = int(time.time() * 1000)
        return self.diagnostics()

    def register_handlers(self):
        from mautrix.client import InternalEventType
        from mautrix.client.dispatcher import MembershipEventDispatcher
        from mautrix.client.encryption_manager import DecryptionDispatcher
        from mautrix.types import EventType

        self.client.remove_dispatcher(DecryptionDispatcher)
        self.client.add_dispatcher(MembershipEventDispatcher)
        kinds = [
            EventType.ROOM_MESSAGE,
            EventType.REACTION,
            EventType.ROOM_REDACTION,
            EventType.ROOM_MEMBER,
            EventType.RECEIPT,
            EventType.TYPING,
            EventType.PRESENCE,
        ]
        for kind in kinds:
            self.client.add_event_handler(kind, self.handle_event)
        self.client.add_event_handler(EventType.ROOM_ENCRYPTED, self.handle_encrypted_event)
        self.client.add_event_handler(InternalEventType.INVITE, self.handle_event)

    async def handle_event(self, event):
        value = serialize_event(event)
        room_id = value.get("room_id")
        if isinstance(room_id, str) and room_id:
            value["room"] = await self.room_snapshot(room_id)
        await self.on_event(value)

    async def handle_encrypted_event(self, event):
        encrypted = serialize_event(event)
        if self.crypto.enabled:
            try:
                decrypted = await self.crypto.machine.decrypt_megolm_event(event)
                value = serialize_event(decrypted)
                value["encrypted_source"] = encrypted
                room_id = value.get("room_id")
                if isinstance(room_id, str) and room_id:
                    value["room"] = await self.room_snapshot(room_id)
                await self.on_event(value)
                return
            except Exception as error:
                encrypted["decryption"] = {
                    "decrypted": False,
                    "error": "Matrix encrypted event decryption failed",
                    "details": external_error_observation(error),
                }
        else:
            encrypted["decryption"] = {"decrypted": False, "error": "Matrix E2EE is unavailable"}
        room_id = encrypted.get("room_id")
        if isinstance(room_id, str) and room_id:
            encrypted["room"] = await self.room_snapshot(room_id)
        await self.on_event(encrypted)

    async def sync(self, since, full_state=False):
        if self.client is None:
            raise MatrixWireFailure("not_connected", "Matrix client is not connected")
        try:
            params = {"timeout": self.config["transport"]["sync_timeout_ms"], "full_state": "true" if full_state else "false"}
            if since:
                params["since"] = since
            path = "/_matrix/client/v3/sync?" + urllib.parse.urlencode(params)
            value = await asyncio.wait_for(
                self.http_json("GET", path, safe_retry=True),
                timeout=self.config["transport"]["sync_watchdog_ms"] / 1000.0,
            )
        except asyncio.TimeoutError as error:
            raise MatrixWireFailure("sync_watchdog_timeout", "Matrix sync exceeded its watchdog") from error
        except Exception as error:
            raise self.translate_error("sync_failed", error)
        if not isinstance(value, dict):
            message = getattr(value, "message", None)
            raise MatrixWireFailure("sync_malformed", str(message or f"unexpected sync response {type(value).__name__}"))
        next_batch = value.get("next_batch")
        if not isinstance(next_batch, str) or not next_batch:
            raise MatrixWireFailure("sync_malformed", "Matrix sync response has no next_batch token")
        joined = value.get("rooms", {}).get("join", {}) if isinstance(value.get("rooms"), dict) else {}
        if isinstance(joined, dict):
            self.joined_rooms.update(str(room_id) for room_id in joined)
        from mautrix.types import SyncResponse

        tasks = self.client.handle_sync(SyncResponse.deserialize(value))
        if inspect.isawaitable(tasks):
            tasks = await tasks
        if tasks:
            await asyncio.gather(*tasks)
        self.last_sync_unix_ms = int(time.time() * 1000)
        if time.monotonic() - self.last_dm_refresh >= 60.0:
            await self.refresh_dm_rooms()
        return value

    async def refresh_dm_rooms(self):
        if self.client is None:
            return
        try:
            value = await self.client.get_account_data("m.direct")
            value = json_value(getattr(value, "content", value))
        except Exception:
            return
        if not isinstance(value, dict):
            return
        rooms = set()
        for entries in value.values():
            if isinstance(entries, list):
                rooms.update(item for item in entries if isinstance(item, str) and ROOM_ID_RE.fullmatch(item))
        if rooms != self.dm_rooms:
            self.dm_rooms = rooms
            self.room_cache.clear()
            self.room_cache_time.clear()
        self.last_dm_refresh = time.monotonic()

    async def room_snapshot(self, room_id):
        cached = self.room_cache.get(room_id)
        cached_at = self.room_cache_time.get(room_id, 0.0)
        if cached is not None and time.monotonic() - cached_at < 60.0:
            return cached
        name = None
        topic = None
        alias = None
        members = None
        encrypted = None
        if self.client is not None:
            for event_type, key in (("m.room.name", "name"), ("m.room.topic", "topic"), ("m.room.canonical_alias", "alias")):
                try:
                    value = json_value(await self.client.get_state_event(room_id, event_type))
                    if isinstance(value, dict) and isinstance(value.get(key), str):
                        if key == "name":
                            name = value[key]
                        elif key == "topic":
                            topic = value[key]
                        else:
                            alias = value[key]
                except Exception:
                    pass
            try:
                members = len(await self.client.get_joined_members(room_id))
            except Exception:
                members = None
            try:
                encrypted = await self.crypto.room_encrypted(room_id)
            except Exception:
                encrypted = None
        direct = room_id in self.dm_rooms or (members is not None and members <= 2)
        result = {
            "name": name,
            "topic": topic,
            "canonical_alias": alias,
            "joined_member_count": members,
            "is_direct": direct,
            "encrypted": encrypted,
            "display_name": name or alias or room_id,
            "server_name": room_id.rsplit(":", 1)[-1] if ":" in room_id else None,
        }
        self.room_cache[room_id] = result
        self.room_cache_time[room_id] = time.monotonic()
        return result

    async def fetch_event(self, room_id, event_id):
        from mautrix.types import EventID, RoomID

        value = await self.safe_call(
            "event.get",
            lambda: self.client.get_event(RoomID(require_room(room_id, "Matrix event room_id")), EventID(require_event(event_id, "Matrix event id"))),
            True,
        )
        result = serialize_event(value)
        result.setdefault("room_id", room_id)
        return result

    def translate_error(self, category, error):
        if isinstance(error, MatrixWireFailure):
            return error
        status = getattr(error, "status", None) or getattr(error, "status_code", None)
        if isinstance(status, bool) or not isinstance(status, int):
            status = None
        errcode = matrix_errcode(getattr(error, "errcode", None))
        retry_after = getattr(error, "retry_after_ms", None)
        message = str(error)
        if errcode is None:
            match = re.search(r"\b(M_[A-Z0-9_]+)\b", message)
            errcode = matrix_errcode(match.group(1)) if match else None
        if retry_after is None:
            match = re.search(r"retry[_ -]?after[^0-9]*([0-9]+)", message, re.IGNORECASE)
            retry_after = int(match.group(1)) if match else None
        if isinstance(retry_after, bool) or not isinstance(retry_after, int) or retry_after < 0:
            retry_after = None
        if status in {401, 403} or errcode in {"M_UNKNOWN_TOKEN", "M_FORBIDDEN"}:
            category = "authorization_failed"
        elif status == 429 or errcode == "M_LIMIT_EXCEEDED":
            category = "rate_limited"
        elif errcode == "M_NOT_FOUND":
            category = "not_found"
        if status is not None:
            safe_message = f"Matrix request failed with HTTP {status}"
        elif errcode is not None:
            safe_message = f"Matrix request failed with {errcode}"
        else:
            safe_message = "Matrix request failed"
        return MatrixWireFailure(
            category,
            safe_message,
            status=status,
            errcode=errcode,
            retry_after_ms=retry_after,
            details=external_error_observation(error),
        )

    async def safe_call(self, operation, factory, safe_retry):
        attempts = 0
        delay = self.config["transport"]["reconnect_backoff_ms"]
        maximum_wait = self.config["transport"]["rate_limit_max_wait_ms"]
        while True:
            try:
                return await factory()
            except CryptoFailure:
                raise
            except Exception as error:
                translated = self.translate_error(f"{operation}_failed", error)
                attempts += 1
                if not safe_retry or attempts > self.config["transport"]["safe_retry_limit"]:
                    raise translated from error
                if translated.category == "authorization_failed":
                    raise translated from error
                retryable = (
                    translated.category == "rate_limited"
                    or (translated.status is not None and translated.status >= 500)
                    or (translated.status is None and translated.errcode is None)
                )
                if not retryable:
                    raise translated from error
                wait = translated.retry_after_ms if translated.retry_after_ms is not None else delay
                if wait > maximum_wait:
                    raise MatrixWireFailure("rate_limit_wait_exceeded", f"Matrix retry wait {wait} exceeds configured ceiling", details={"operation": operation}) from error
                await asyncio.sleep(wait / 1000.0)
                delay = min(delay * 2, self.config["transport"]["reconnect_backoff_cap_ms"])

    async def send_event(self, room_id, kind, content, seed, safe_retry=True):
        from mautrix.types import EventType, RoomID

        if await self.crypto.room_encrypted(room_id) and not self.crypto.enabled:
            raise CryptoFailure("encrypted_room_unavailable", "cannot send an event to an encrypted Matrix room without crypto")
        txn = transaction_id(seed)
        event_type = EventType.find(kind)
        value = await self.safe_call(
            "send_event",
            lambda: self.client.send_message_event(RoomID(room_id), event_type, content, txn_id=txn),
            safe_retry,
        )
        return {"event_id": str(value), "transaction_id": txn, "room_id": room_id, "event_type": kind}

    async def execute(self, operation, args, seed):
        if self.client is None:
            raise MatrixWireFailure("not_connected", "Matrix client is not connected")
        if operation not in self.config["outbound"]["allowed_operations"]:
            raise MatrixWireFailure("operation_not_declared", f"Matrix operation {operation!r} is not declared")
        safe_retry = operation in self.config["outbound"]["retry_safe_operations"]
        try:
            return await self._execute(operation, require_object(args, "Matrix operation args"), seed, safe_retry)
        except (MatrixWireFailure, CryptoFailure):
            raise
        except ValueError:
            raise
        except Exception as error:
            raise self.translate_error(f"{operation}_failed", error) from error

    async def _execute(self, operation, args, seed, safe_retry):
        from mautrix.types import EventID, EventType, PaginationDirection, PresenceState, RoomCreatePreset, RoomDirectoryVisibility, RoomID, SyncToken, UserID

        if operation == "message.send":
            args = require_keys(args, {"text", "room_id"}, {"thread_event_id", "reply_to_event_id", "formatted_html", "notice"}, "Matrix message.send")
            room_id = require_room(args["room_id"], "Matrix message.send.room_id")
            text = require_string(args["text"], "Matrix message.send.text", allow_empty=True, maximum=1048576)
            if not text:
                raise ValueError("Matrix message.send.text must not be empty")
            thread = require_event(args.get("thread_event_id"), "Matrix message.send.thread_event_id", nullable=True)
            reply = require_event(args.get("reply_to_event_id"), "Matrix message.send.reply_to_event_id", nullable=True)
            explicit_html = args.get("formatted_html")
            if explicit_html is not None:
                explicit_html = require_string(explicit_html, "Matrix message.send.formatted_html", allow_empty=True, maximum=4194304)
            notice = require_bool(args.get("notice", False), "Matrix message.send.notice")
            chunks = split_text(text, self.config["outbound"]["max_message_chars"])
            event_ids = []
            for index, chunk in enumerate(chunks):
                content = build_text_content(chunk, self.config["gating"]["allow_room_mentions"], notice, explicit_html if len(chunks) == 1 else None)
                apply_relation(content, thread, reply if index == 0 else None)
                result = await self.send_event(room_id, "m.room.message", content, f"{seed}:message:{index}", True)
                event_ids.append(result["event_id"])
            return {"room_id": room_id, "event_ids": event_ids, "message_id": event_ids[-1], "chunks": len(event_ids)}
        if operation == "message.edit":
            args = require_keys(args, {"room_id", "event_id", "text"}, {"thread_event_id", "formatted_html", "notice"}, "Matrix message.edit")
            room_id = require_room(args["room_id"], "Matrix message.edit.room_id")
            event_id = require_event(args["event_id"], "Matrix message.edit.event_id")
            text = require_string(args["text"], "Matrix message.edit.text", maximum=self.config["outbound"]["max_message_chars"])
            notice = require_bool(args.get("notice", False), "Matrix message.edit.notice")
            explicit = args.get("formatted_html")
            if explicit is not None:
                explicit = require_string(explicit, "Matrix message.edit.formatted_html", allow_empty=True, maximum=4194304)
            new_content = build_text_content(text, self.config["gating"]["allow_room_mentions"], notice, explicit)
            content = {"msgtype": new_content["msgtype"], "body": f"* {text}", "m.new_content": new_content, "m.relates_to": {"rel_type": "m.replace", "event_id": event_id}}
            if "m.mentions" in new_content:
                content["m.mentions"] = new_content["m.mentions"]
            if "formatted_body" in new_content:
                content["format"] = "org.matrix.custom.html"
                content["formatted_body"] = "* " + new_content["formatted_body"]
            result = await self.send_event(room_id, "m.room.message", content, f"{seed}:edit", True)
            return {"room_id": room_id, "event_id": result["event_id"], "replaces_event_id": event_id}
        if operation == "location.send":
            args = require_keys(args, {"room_id", "geo_uri"}, {"description", "thread_event_id", "reply_to_event_id"}, "Matrix location.send")
            room_id = require_room(args["room_id"], "Matrix location.send.room_id")
            geo_uri = require_string(args["geo_uri"], "Matrix location.send.geo_uri", maximum=2048)
            coordinates = re.fullmatch(r"geo:(-?[0-9]+(?:\.[0-9]+)?),(-?[0-9]+(?:\.[0-9]+)?)(?:;[^\s]+)?", geo_uri)
            if coordinates is None or not -90.0 <= float(coordinates.group(1)) <= 90.0 or not -180.0 <= float(coordinates.group(2)) <= 180.0:
                raise ValueError("Matrix location.send.geo_uri is malformed")
            description = require_string(args.get("description", "Location"), "Matrix location.send.description", maximum=4096)
            content = {"msgtype": "m.location", "body": description, "geo_uri": geo_uri, "m.location": {"uri": geo_uri, "description": description}}
            apply_relation(content, require_event(args.get("thread_event_id"), "Matrix location.send.thread_event_id", nullable=True), require_event(args.get("reply_to_event_id"), "Matrix location.send.reply_to_event_id", nullable=True))
            return await self.send_event(room_id, "m.room.message", content, f"{seed}:location", True)
        if operation == "reaction.send":
            args = require_keys(args, {"room_id", "event_id", "key"}, set(), "Matrix reaction.send")
            room_id = require_room(args["room_id"], "Matrix reaction.send.room_id")
            event_id = require_event(args["event_id"], "Matrix reaction.send.event_id")
            key = require_string(args["key"], "Matrix reaction.send.key", maximum=256)
            content = {"m.relates_to": {"rel_type": "m.annotation", "event_id": event_id, "key": key}}
            result = await self.send_event(room_id, "m.reaction", content, f"{seed}:reaction", True)
            result["reacts_to_event_id"] = event_id
            result["key"] = key
            return result
        if operation in {"message.redact", "reaction.redact"}:
            args = require_keys(args, {"room_id", "event_id"}, {"reason"}, f"Matrix {operation}")
            room_id = require_room(args["room_id"], f"Matrix {operation}.room_id")
            event_id = require_event(args["event_id"], f"Matrix {operation}.event_id")
            reason = require_string(args.get("reason", ""), f"Matrix {operation}.reason", allow_empty=True, maximum=4096)
            txn = transaction_id(f"{seed}:redact")
            path = f"/_matrix/client/v3/rooms/{urllib.parse.quote(room_id, safe='')}/redact/{urllib.parse.quote(event_id, safe='')}/{txn}"
            response = await self.http_json("PUT", path, {"reason": reason} if reason else {}, safe_retry=True)
            return {"room_id": room_id, "redacted_event_id": event_id, "event_id": response.get("event_id"), "transaction_id": txn}
        if operation in {"typing.set", "typing.clear"}:
            required = {"room_id"}
            optional = {"timeout_ms"} if operation == "typing.set" else set()
            args = require_keys(args, required, optional, f"Matrix {operation}")
            room_id = require_room(args["room_id"], f"Matrix {operation}.room_id")
            timeout = require_int(args.get("timeout_ms", 30000), f"Matrix {operation}.timeout_ms", 1, 300000) if operation == "typing.set" else 0
            await self.safe_call(operation, lambda: self.client.set_typing(RoomID(room_id), timeout=timeout), True)
            return {"room_id": room_id, "typing": operation == "typing.set", "timeout_ms": timeout}
        if operation == "receipt.read":
            args = require_keys(args, {"room_id", "event_id"}, set(), "Matrix receipt.read")
            room_id = require_room(args["room_id"], "Matrix receipt.read.room_id")
            event_id = require_event(args["event_id"], "Matrix receipt.read.event_id")
            await self.safe_call(operation, lambda: self.client.set_fully_read_marker(RoomID(room_id), EventID(event_id), EventID(event_id)), True)
            return {"room_id": room_id, "event_id": event_id, "receipt": "m.read", "fully_read": True}
        if operation == "presence.set":
            args = require_keys(args, {"state"}, {"status"}, "Matrix presence.set")
            state = require_string(args["state"], "Matrix presence.set.state")
            states = {"online": PresenceState.ONLINE, "offline": PresenceState.OFFLINE, "unavailable": PresenceState.UNAVAILABLE}
            if state not in states:
                raise ValueError("Matrix presence state is invalid")
            status = require_string(args.get("status", ""), "Matrix presence.set.status", allow_empty=True, maximum=255)
            await self.safe_call(operation, lambda: self.client.set_presence(states[state], status or None), False)
            return {"state": state, "status": status}
        if operation == "room.create":
            args = require_keys(args, set(), {"name", "topic", "invite", "is_direct", "visibility", "preset", "alias_localpart", "initial_state"}, "Matrix room.create")
            visibility = require_string(args.get("visibility", "private"), "Matrix room.create.visibility")
            if visibility not in {"private", "public"}:
                raise ValueError("Matrix room visibility is invalid")
            if visibility == "public" and not self.config["authz"]["allow_public_room_creation"]:
                raise ValueError("Matrix public room creation is disabled")
            preset_name = require_string(args.get("preset", "private_chat"), "Matrix room.create.preset")
            presets = {"private_chat": RoomCreatePreset.PRIVATE, "public_chat": RoomCreatePreset.PUBLIC, "trusted_private_chat": RoomCreatePreset.TRUSTED_PRIVATE}
            if preset_name not in presets:
                raise ValueError("Matrix room preset is invalid")
            invite = [UserID(require_mxid(value, "Matrix room.create.invite entry")) for value in require_string_list(args.get("invite", []), "Matrix room.create.invite", True, 1000)]
            initial_state = args.get("initial_state", [])
            if not isinstance(initial_state, list) or any(not isinstance(item, dict) for item in initial_state) or len(initial_state) > 100:
                raise ValueError("Matrix room.create.initial_state must be an object array with at most 100 entries")
            result = await self.safe_call(
                operation,
                lambda: self.client.create_room(
                    alias_localpart=require_string(args.get("alias_localpart", ""), "Matrix room.create.alias_localpart", allow_empty=True, maximum=255) or None,
                    visibility=RoomDirectoryVisibility.PUBLIC if visibility == "public" else RoomDirectoryVisibility.PRIVATE,
                    preset=presets[preset_name],
                    name=require_string(args.get("name", ""), "Matrix room.create.name", allow_empty=True, maximum=255) or None,
                    topic=require_string(args.get("topic", ""), "Matrix room.create.topic", allow_empty=True, maximum=4096) or None,
                    is_direct=require_bool(args.get("is_direct", False), "Matrix room.create.is_direct"),
                    invitees=invite,
                    initial_state=initial_state,
                ),
                False,
            )
            room_id = str(result)
            self.joined_rooms.add(room_id)
            return {"room_id": room_id, "visibility": visibility, "preset": preset_name}
        if operation == "room.join":
            args = require_keys(args, {"room_id"}, {"servers"}, "Matrix room.join")
            room = require_room(args["room_id"], "Matrix room.join.room_id")
            servers = require_string_list(args.get("servers", []), "Matrix room.join.servers", True, 20)
            result = await self.safe_call(operation, lambda: self.client.join_room(room, servers=servers or None), False)
            room_id = str(result)
            self.joined_rooms.add(room_id)
            self.room_cache.pop(room_id, None)
            return {"room_id": room_id, "joined": True}
        if operation == "room.leave":
            args = require_keys(args, {"room_id"}, {"reason"}, "Matrix room.leave")
            room_id = require_room(args["room_id"], "Matrix room.leave.room_id")
            reason = require_string(args.get("reason", ""), "Matrix room.leave.reason", allow_empty=True, maximum=4096)
            await self.safe_call(operation, lambda: self.client.leave_room(RoomID(room_id), reason=reason or None), False)
            self.joined_rooms.discard(room_id)
            return {"room_id": room_id, "left": True}
        if operation in {"room.invite", "room.kick", "room.ban", "room.unban"}:
            args = require_keys(args, {"room_id", "user_id"}, {"reason"}, f"Matrix {operation}")
            room_id = require_room(args["room_id"], f"Matrix {operation}.room_id")
            user_id = require_mxid(args["user_id"], f"Matrix {operation}.user_id")
            reason = require_string(args.get("reason", ""), f"Matrix {operation}.reason", allow_empty=True, maximum=4096)
            method = {"room.invite": self.client.invite_user, "room.kick": self.client.kick_user, "room.ban": self.client.ban_user, "room.unban": self.client.unban_user}[operation]
            await self.safe_call(operation, lambda: method(RoomID(room_id), UserID(user_id), reason=reason or None), False)
            return {"room_id": room_id, "user_id": user_id, "operation": operation}
        if operation in {"room.name.set", "room.topic.set"}:
            key = "name" if operation == "room.name.set" else "topic"
            args = require_keys(args, {"room_id", key}, set(), f"Matrix {operation}")
            room_id = require_room(args["room_id"], f"Matrix {operation}.room_id")
            value = require_string(args[key], f"Matrix {operation}.{key}", allow_empty=True, maximum=4096)
            kind = "m.room.name" if key == "name" else "m.room.topic"
            event_id = await self.safe_call(operation, lambda: self.client.send_state_event(RoomID(room_id), EventType.find(kind), {key: value}), False)
            self.room_cache.pop(room_id, None)
            return {"room_id": room_id, "event_id": str(event_id), key: value}
        if operation == "room.state.get":
            args = require_keys(args, {"room_id", "event_type"}, {"state_key"}, "Matrix room.state.get")
            room_id = require_room(args["room_id"], "Matrix room.state.get.room_id")
            event_type = require_string(args["event_type"], "Matrix room.state.get.event_type", maximum=255)
            state_key = require_string(args.get("state_key", ""), "Matrix room.state.get.state_key", allow_empty=True, maximum=255)
            value = await self.safe_call(operation, lambda: self.client.get_state_event(RoomID(room_id), EventType.find(event_type), state_key=state_key), True)
            return {"room_id": room_id, "event_type": event_type, "state_key": state_key, "content": json_value(value)}
        if operation == "room.members":
            args = require_keys(args, {"room_id"}, set(), "Matrix room.members")
            room_id = require_room(args["room_id"], "Matrix room.members.room_id")
            value = await self.safe_call(operation, lambda: self.client.get_joined_members(RoomID(room_id)), True)
            result = {}
            for user_id, member in value.items():
                result[str(user_id)] = json_value(member)
            return {"room_id": room_id, "members": result}
        if operation == "room.history":
            args = require_keys(args, {"room_id"}, {"limit", "from_token"}, "Matrix room.history")
            room_id = require_room(args["room_id"], "Matrix room.history.room_id")
            limit = require_int(args.get("limit", 20), "Matrix room.history.limit", 1, 1000)
            from_token = require_string(args.get("from_token", ""), "Matrix room.history.from_token", allow_empty=True, maximum=4096)
            value = await self.safe_call(operation, lambda: self.client.get_messages(RoomID(room_id), PaginationDirection.BACKWARD, SyncToken(from_token) if from_token else None, limit=limit), True)
            chunk = getattr(value, "events", None) or getattr(value, "chunk", None) or []
            return {"room_id": room_id, "events": [serialize_event(event) for event in chunk], "start": str(getattr(value, "start", "") or ""), "end": str(getattr(value, "end", "") or "")}
        if operation == "account.data.get":
            args = require_keys(args, {"event_type"}, {"room_id"}, "Matrix account.data.get")
            event_type = require_string(args["event_type"], "Matrix account.data.get.event_type", maximum=255)
            room_id = args.get("room_id")
            if room_id is not None:
                room_id = require_room(room_id, "Matrix account.data.get.room_id")
            value = await self.safe_call(operation, lambda: self.client.get_account_data(event_type, RoomID(room_id) if room_id else None), True)
            return {"event_type": event_type, "room_id": room_id, "content": json_value(value)}
        if operation == "device.keys.query":
            args = require_keys(args, {"user_id"}, set(), "Matrix device.keys.query")
            user_id = require_mxid(args["user_id"], "Matrix device.keys.query.user_id")
            return {"user_id": user_id, "devices": await self.crypto.query_devices(user_id)}
        if operation == "device.verify":
            args = require_keys(args, {"user_id", "device_id", "trust"}, set(), "Matrix device.verify")
            user_id = require_mxid(args["user_id"], "Matrix device.verify.user_id")
            device_id = require_string(args["device_id"], "Matrix device.verify.device_id", maximum=255)
            trust = require_string(args["trust"], "Matrix device.verify.trust", maximum=32)
            return await self.crypto.verify_device(user_id, device_id, trust)
        if operation == "media.send":
            return await self.send_media(args, seed)
        raise MatrixWireFailure("operation_unimplemented", f"Matrix operation {operation!r} is declared but not implemented")

    async def send_media(self, args, seed):
        args = require_keys(
            args,
            {"room_id", "data_base64", "filename", "mimetype", "msgtype"},
            {"caption", "thread_event_id", "reply_to_event_id", "voice", "info"},
            "Matrix media.send",
        )
        room_id = require_room(args["room_id"], "Matrix media.send.room_id")
        filename = require_string(args["filename"], "Matrix media.send.filename", maximum=1024)
        if filename != os.path.basename(filename):
            raise ValueError("Matrix media filename must not contain a path")
        mimetype = require_string(args["mimetype"], "Matrix media.send.mimetype", maximum=255)
        msgtype = require_string(args["msgtype"], "Matrix media.send.msgtype")
        if msgtype not in {"m.image", "m.audio", "m.video", "m.file"}:
            raise ValueError("Matrix media msgtype is invalid")
        encoded = require_string(args["data_base64"], "Matrix media.send.data_base64", maximum=((self.config["media"]["max_upload_bytes"] + 2) // 3) * 4 + 16)
        try:
            data = base64.b64decode(encoded, validate=True)
        except Exception as error:
            raise ValueError("Matrix media data_base64 is invalid") from error
        if len(data) > self.config["media"]["max_upload_bytes"]:
            raise ValueError("Matrix media exceeds the upload byte ceiling")
        upload, encrypted_descriptor = await self.crypto.encrypted_media(room_id, data)
        uri = await self.safe_call(
            "media.upload",
            lambda: self.client.upload_media(upload, mime_type=mimetype, filename=filename, size=len(upload)),
            False,
        )
        info = args.get("info") or {}
        if not isinstance(info, dict):
            raise ValueError("Matrix media info must be an object")
        if len(json.dumps(info, ensure_ascii=False, separators=(",", ":")).encode("utf-8")) > 1048576:
            raise ValueError("Matrix media info exceeds one mebibyte")
        info = dict(info)
        info.update({"mimetype": mimetype, "size": len(data)})
        content = {"msgtype": msgtype, "body": require_string(args.get("caption", filename), "Matrix media.send.caption", maximum=4096), "info": info}
        if encrypted_descriptor is None:
            content["url"] = str(uri)
        else:
            encrypted_descriptor["url"] = str(uri)
            content["file"] = encrypted_descriptor
        if require_bool(args.get("voice", False), "Matrix media.send.voice"):
            if msgtype != "m.audio":
                raise ValueError("Matrix native voice flag requires m.audio")
            content["org.matrix.msc3245.voice"] = {}
        apply_relation(content, require_event(args.get("thread_event_id"), "Matrix media.send.thread_event_id", nullable=True), require_event(args.get("reply_to_event_id"), "Matrix media.send.reply_to_event_id", nullable=True))
        result = await self.send_event(room_id, "m.room.message", content, f"{seed}:media", True)
        result.update({"mxc_uri": str(uri), "filename": filename, "mimetype": mimetype, "size": len(data), "encrypted": encrypted_descriptor is not None})
        return result

    async def download_media(self, content):
        content = require_object(content, "Matrix inbound media content")
        descriptor = content.get("file") if isinstance(content.get("file"), dict) else None
        uri = descriptor.get("url") if descriptor else content.get("url")
        if not isinstance(uri, str) or not re.fullmatch(r"mxc://[^/\s]+/[^\s]+", uri):
            raise MatrixWireFailure("media_uri_invalid", "Matrix inbound media does not contain a valid MXC URI")
        server, media_id = uri[6:].split("/", 1)
        path = "/_matrix/client/v1/media/download/" + urllib.parse.quote(server, safe="") + "/" + urllib.parse.quote(media_id, safe="")
        data = await self.download_media_bytes(path)
        if descriptor is not None:
            data = await self.crypto.decrypt_media(descriptor, data)
            if len(data) > self.config["media"]["max_download_bytes"]:
                raise MatrixWireFailure("media_too_large", "decrypted Matrix media exceeds the download byte ceiling", details={"size": len(data)})
        return uri, data

    async def download_media_bytes(self, path):
        if self.session is None:
            raise MatrixWireFailure("not_connected", "Matrix HTTP session is not connected")
        maximum = self.config["media"]["max_download_bytes"]
        download_root = os.path.join(self.var_dir, "media-downloads")
        os.makedirs(download_root, mode=0o700, exist_ok=True)
        directory_state = os.lstat(download_root)
        if stat.S_ISLNK(directory_state.st_mode) or not stat.S_ISDIR(directory_state.st_mode) or directory_state.st_uid != os.getuid():
            raise MatrixWireFailure("media_storage_invalid", "Matrix media download directory is not owner-controlled")
        if stat.S_IMODE(directory_state.st_mode) != 0o700:
            os.chmod(download_root, 0o700)
        descriptor, temporary = tempfile.mkstemp(prefix="matrix-media-", suffix=".part", dir=download_root)
        os.chmod(temporary, 0o600)
        url = self.homeserver + path
        headers = {"Authorization": f"Bearer {self.access_token or self.api.token}"}
        total = 0
        try:
            with os.fdopen(descriptor, "wb") as output:
                descriptor = None
                async with self.session.get(url, headers=headers, allow_redirects=False) as response:
                    if response.status < 200 or response.status >= 300:
                        raw = await response.content.read(min(maximum, 4096) + 1)
                        raise MatrixWireFailure(
                            "media_download_failed",
                            f"Matrix media download returned HTTP {response.status}",
                            status=response.status,
                            details=body_observation(raw),
                        )
                    declared = response.headers.get("Content-Length")
                    if declared is not None:
                        try:
                            declared_size = int(declared)
                        except ValueError as error:
                            raise MatrixWireFailure("media_download_malformed", "Matrix media Content-Length is invalid") from error
                        if declared_size < 0 or declared_size > maximum:
                            raise MatrixWireFailure("media_too_large", "Matrix media exceeds the download byte ceiling", details={"declared_size": declared_size})
                    while True:
                        chunk = await response.content.read(min(65536, maximum + 1 - total))
                        if not chunk:
                            break
                        total += len(chunk)
                        if total > maximum:
                            raise MatrixWireFailure("media_too_large", "Matrix media exceeds the download byte ceiling", details={"observed_size": total})
                        output.write(chunk)
                output.flush()
                os.fsync(output.fileno())
            with open(temporary, "rb") as handle:
                return handle.read(maximum + 1)
        except MatrixWireFailure:
            raise
        except Exception as error:
            raise self.translate_error("media_download_failed", error) from error
        finally:
            if descriptor is not None:
                os.close(descriptor)
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass

    async def http_json(self, method, path, body=None, safe_retry=False):
        if self.session is None:
            raise MatrixWireFailure("not_connected", "Matrix HTTP session is not connected")
        url = self.homeserver + path
        attempts = 0
        delay = self.config["transport"]["reconnect_backoff_ms"]
        while True:
            try:
                headers = {"Authorization": f"Bearer {self.access_token or self.api.token}", "Content-Type": "application/json"}
                async with self.session.request(method, url, headers=headers, json=body) as response:
                    raw = await response.content.read(self.config["transport"]["max_response_bytes"] + 1)
                    if len(raw) > self.config["transport"]["max_response_bytes"]:
                        raise MatrixWireFailure("response_too_large", "Matrix response exceeds configured byte ceiling", status=response.status)
                    try:
                        value = json.loads(raw) if raw else {}
                    except json.JSONDecodeError as error:
                        raise MatrixWireFailure("malformed_response", "Matrix response is not JSON", status=response.status) from error
                    if not isinstance(value, dict):
                        raise MatrixWireFailure("malformed_response", "Matrix response root is not an object", status=response.status)
                    if response.status < 200 or response.status >= 300:
                        retry_after = value.get("retry_after_ms")
                        if isinstance(retry_after, bool) or not isinstance(retry_after, int) or retry_after < 0:
                            retry_after = None
                        errcode = matrix_errcode(value.get("errcode"))
                        raise MatrixWireFailure(
                            "rate_limited" if response.status == 429 else "http_error",
                            f"Matrix HTTP request failed with status {response.status}",
                            status=response.status,
                            errcode=errcode,
                            retry_after_ms=retry_after,
                            details=body_observation(raw),
                        )
                    return value
            except MatrixWireFailure as error:
                attempts += 1
                retryable = error.category == "rate_limited" or (error.status is not None and error.status >= 500)
                if not safe_retry or attempts > self.config["transport"]["safe_retry_limit"] or not retryable:
                    raise
                wait = error.retry_after_ms if error.retry_after_ms is not None else delay
                if wait > self.config["transport"]["rate_limit_max_wait_ms"]:
                    raise MatrixWireFailure("rate_limit_wait_exceeded", "Matrix HTTP retry wait exceeds configured ceiling") from error
                await asyncio.sleep(wait / 1000.0)
                delay = min(delay * 2, self.config["transport"]["reconnect_backoff_cap_ms"])
            except Exception as error:
                attempts += 1
                if not safe_retry or attempts > self.config["transport"]["safe_retry_limit"]:
                    raise self.translate_error("transport_error", error) from error
                await asyncio.sleep(delay / 1000.0)
                delay = min(delay * 2, self.config["transport"]["reconnect_backoff_cap_ms"])

    def diagnostics(self):
        return {
            "homeserver": self.homeserver,
            "user_id": self.user_id,
            "device_id_present": bool(self.device_id),
            "connected": self.client is not None,
            "connected_unix_ms": self.connected_unix_ms,
            "last_sync_unix_ms": self.last_sync_unix_ms,
            "joined_room_count": len(self.joined_rooms),
            "direct_room_count": len(self.dm_rooms),
            "crypto": self.crypto.diagnostics(),
        }

    async def close(self):
        await self.crypto.close()
        if self.session is not None:
            session = self.session
            self.session = None
            try:
                await session.close()
            except Exception:
                pass
        self.client = None
        self.api = None
