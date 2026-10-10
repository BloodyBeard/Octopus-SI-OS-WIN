#!/usr/bin/env python3
import fcntl
import hashlib
import importlib.metadata
import ipaddress
import json
import os
import platform
import re
import shutil
import stat
import subprocess
import sys
import time
import urllib.parse


ROOT = os.path.dirname(os.path.abspath(__file__))
VAR = os.path.abspath(os.environ["OCTOPUS_VAR_DIR"])
CONFIG = os.path.join(ROOT, "config.json")
REQUIREMENTS = os.path.join(ROOT, "requirements.txt")
REQUIREMENTS_LOCK = os.path.join(ROOT, "requirements.lock.txt")
LIFE = os.path.join(ROOT, "discord_life.py")
RUNTIME_ROOT = os.path.join(VAR, "python-runtime")
LOCK_PATH = os.path.join(VAR, "python-runtime.lock")
DEPENDENCY_ENV_ALLOWLIST = (
    "PATH",
    "TMPDIR",
    "TMP",
    "TEMP",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
)


def require_private_directory(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    info = os.lstat(path)
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
        raise RuntimeError(f"{path} must be a private directory")
    if stat.S_IMODE(info.st_mode) != 0o700:
        os.chmod(path, 0o700)
    if hasattr(os, "geteuid") and info.st_uid != os.geteuid():
        raise RuntimeError(f"{path} must be owned by the interface process user")


def read_json(path):
    with open(path, "rb") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise RuntimeError(f"{path} must contain a JSON object")
    return value


def dependency_config():
    value = read_json(CONFIG).get("dependencies")
    if not isinstance(value, dict) or set(value) != {"install_timeout_ms", "keep_versions"}:
        raise RuntimeError("discord dependency config is malformed")
    timeout = value["install_timeout_ms"]
    keep = value["keep_versions"]
    if isinstance(timeout, bool) or not isinstance(timeout, int) or not 1000 <= timeout <= 3600000:
        raise RuntimeError("discord dependency install timeout is invalid")
    if isinstance(keep, bool) or not isinstance(keep, int) or not 1 <= keep <= 16:
        raise RuntimeError("discord dependency retention is invalid")
    return timeout, keep


def dependency_identity():
    digest = hashlib.sha256()
    for path in (REQUIREMENTS, REQUIREMENTS_LOCK):
        with open(path, "rb") as handle:
            digest.update(handle.read())
    digest.update(sys.implementation.name.encode("utf-8"))
    digest.update(f"{sys.version_info.major}.{sys.version_info.minor}".encode("ascii"))
    digest.update(sys.platform.encode("utf-8"))
    digest.update(platform.machine().encode("utf-8"))
    return digest.hexdigest()


def canonical_distribution_name(value):
    return re.sub(r"[-_.]+", "-", value).lower()


def locked_inventory():
    result = {}
    with open(REQUIREMENTS_LOCK, "r", encoding="utf-8") as handle:
        for line in handle:
            match = re.fullmatch(r"([A-Za-z0-9_.-]+)==([^ \\]+) \\\n", line)
            if match is None:
                continue
            name = canonical_distribution_name(match.group(1))
            version = match.group(2)
            if name in result:
                raise RuntimeError(f"discord dependency lock repeats distribution {name}")
            result[name] = version
    if not result:
        raise RuntimeError("discord dependency lock contains no distributions")
    return result


def installed_inventory(target):
    result = {}
    for distribution in importlib.metadata.distributions(path=[target]):
        raw_name = distribution.metadata.get("Name")
        if not isinstance(raw_name, str) or not raw_name:
            raise RuntimeError("discord dependency runtime contains an unnamed distribution")
        name = canonical_distribution_name(raw_name)
        if name in result:
            raise RuntimeError(f"discord dependency runtime repeats distribution {name}")
        result[name] = distribution.version
    return result


def normalize_tree_permissions(target):
    for current, directories, files in os.walk(target, topdown=True, followlinks=False):
        state = os.lstat(current)
        if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
            raise PermissionError("discord dependency runtime contains a non-directory tree node")
        os.chmod(current, 0o700)
        for name in directories:
            path = os.path.join(current, name)
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
                raise PermissionError("discord dependency runtime contains an invalid directory")
        for name in files:
            path = os.path.join(current, name)
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISREG(state.st_mode):
                raise PermissionError("discord dependency runtime contains an invalid file")
            os.chmod(path, 0o600)


def runtime_tree_digest(target):
    digest = hashlib.sha256()
    for current, directories, files in os.walk(target, topdown=True, followlinks=False):
        directories.sort()
        files.sort()
        current_state = os.lstat(current)
        if (
            stat.S_ISLNK(current_state.st_mode)
            or not stat.S_ISDIR(current_state.st_mode)
            or stat.S_IMODE(current_state.st_mode) != 0o700
        ):
            raise PermissionError("discord dependency directory integrity failed")
        relative_current = os.path.relpath(current, target)
        digest.update(b"D\0")
        digest.update(relative_current.encode("utf-8"))
        digest.update(b"\n")
        for name in directories:
            path = os.path.join(current, name)
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
                raise PermissionError("discord dependency directory integrity failed")
        for name in files:
            if relative_current == "." and name == ".octopus-dependencies.json":
                continue
            path = os.path.join(current, name)
            state = os.lstat(path)
            if (
                stat.S_ISLNK(state.st_mode)
                or not stat.S_ISREG(state.st_mode)
                or stat.S_IMODE(state.st_mode) != 0o600
            ):
                raise PermissionError("discord dependency file integrity failed")
            relative = os.path.relpath(path, target)
            digest.update(b"F\0")
            digest.update(relative.encode("utf-8"))
            digest.update(b"\0")
            digest.update(str(state.st_size).encode("ascii"))
            digest.update(b"\0")
            descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
            with os.fdopen(descriptor, "rb") as handle:
                while True:
                    chunk = handle.read(1024 * 1024)
                    if not chunk:
                        break
                    digest.update(chunk)
            digest.update(b"\n")
    return digest.hexdigest()


def inventory_matches(target):
    return installed_inventory(target) == locked_inventory()


def life_environment_for(target):
    environment = os.environ.copy()
    environment["PYTHONPATH"] = target
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    environment["PIP_DISABLE_PIP_VERSION_CHECK"] = "1"
    environment["OCTOPUS_CONTENT_HASH_RUNTIME"] = target
    return environment


def dependency_proxy_url():
    api = read_json(CONFIG).get("discord_api")
    if not isinstance(api, dict):
        raise RuntimeError("discord API config is malformed")
    name = api.get("proxy_env")
    if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name):
        raise RuntimeError("discord dependency proxy environment name is malformed")
    value = os.environ.get(name, "").strip()
    if not value:
        return None
    if any(ord(character) <= 32 or ord(character) == 127 for character in value):
        raise RuntimeError("discord dependency proxy URL contains invalid characters")
    try:
        parsed = urllib.parse.urlsplit(value)
        port = parsed.port
    except ValueError as error:
        raise RuntimeError("discord dependency proxy URL is malformed") from error
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise RuntimeError("discord dependency proxy URL must use HTTP or HTTPS with a host")
    if port is not None and not 1 <= port <= 65535:
        raise RuntimeError("discord dependency proxy URL port is invalid")
    if parsed.path not in ("", "/") or parsed.query or parsed.fragment or "?" in value or "#" in value:
        raise RuntimeError("discord dependency proxy URL must not contain a path, query, or fragment")
    host = parsed.hostname.lower()
    try:
        loopback = host == "localhost" or ipaddress.ip_address(host).is_loopback
    except ValueError:
        loopback = False
    if parsed.scheme == "http" and not loopback:
        raise RuntimeError("discord dependency proxy URL must use HTTPS outside loopback")
    return value[:-1] if parsed.path == "/" else value


def dependency_process_environment(proxy_url=None):
    environment = {
        name: os.environ[name]
        for name in DEPENDENCY_ENV_ALLOWLIST
        if os.environ.get(name)
    }
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    environment["PIP_DISABLE_PIP_VERSION_CHECK"] = "1"
    environment["PIP_CONFIG_FILE"] = os.devnull
    if proxy_url:
        environment["HTTP_PROXY"] = proxy_url
        environment["HTTPS_PROXY"] = proxy_url
    return environment


def smoke(target, timeout=60, proxy_url=None):
    code = "import aiohttp,blake3,discord,nacl,numpy;assert blake3.__version__=='1.0.9';assert blake3.blake3(b'abc').hexdigest()=='6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85';assert discord.__version__=='2.7.1';assert nacl.__version__=='1.5.0';assert numpy.__version__=='2.4.3'"
    result = subprocess.run(
        [sys.executable, "-I", "-B", "-c", f"import sys;sys.path.insert(0,{target!r});{code}"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=timeout,
        check=False,
        env=dependency_process_environment(proxy_url),
    )
    return result.returncode == 0


def marker_matches(target, identity, proxy_url):
    try:
        info = os.lstat(target)
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
            return False
        marker = os.path.join(target, ".octopus-dependencies.json")
        marker_info = os.lstat(marker)
        if stat.S_ISLNK(marker_info.st_mode) or not stat.S_ISREG(marker_info.st_mode) or stat.S_IMODE(marker_info.st_mode) != 0o600:
            return False
        value = read_json(marker)
        return (
            set(value) == {"schema_version", "identity", "tree_sha256"}
            and value["schema_version"] == 1
            and value["identity"] == identity
            and isinstance(value["tree_sha256"], str)
            and re.fullmatch(r"[0-9a-f]{64}", value["tree_sha256"]) is not None
            and runtime_tree_digest(target) == value["tree_sha256"]
            and inventory_matches(target)
            and smoke(target, proxy_url=proxy_url)
        )
    except (FileNotFoundError, OSError, RuntimeError, ValueError, subprocess.SubprocessError):
        return False


def write_marker(target, identity, tree_sha256):
    path = os.path.join(target, ".octopus-dependencies.json")
    descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | getattr(os, "O_NOFOLLOW", 0), 0o600)
    with os.fdopen(descriptor, "wb") as handle:
        handle.write(json.dumps({"schema_version": 1, "identity": identity, "tree_sha256": tree_sha256}, sort_keys=True).encode("utf-8"))
        handle.flush()
        os.fsync(handle.fileno())


def install(target, identity, timeout_ms, proxy_url):
    staged = os.path.join(RUNTIME_ROOT, f".{identity}.{os.getpid()}.{time.time_ns()}.tmp")
    os.mkdir(staged, 0o700)
    try:
        result = subprocess.run(
            [
                sys.executable,
                "-I",
                "-m",
                "pip",
                "install",
                "--target",
                staged,
                "--requirement",
                REQUIREMENTS_LOCK,
                "--require-hashes",
                "--only-binary=:all:",
                "--no-input",
                "--no-compile",
                "--no-cache-dir",
                "--no-warn-script-location",
            ],
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=timeout_ms / 1000.0,
            check=False,
            env=dependency_process_environment(proxy_url),
        )
        if result.returncode != 0:
            raise RuntimeError(f"discord dependency installation failed with exit {result.returncode}")
        normalize_tree_permissions(staged)
        actual_inventory = installed_inventory(staged)
        expected_inventory = locked_inventory()
        if actual_inventory != expected_inventory:
            missing = sorted(set(expected_inventory) - set(actual_inventory))
            unexpected = sorted(set(actual_inventory) - set(expected_inventory))
            changed = sorted(name for name in set(expected_inventory) & set(actual_inventory) if expected_inventory[name] != actual_inventory[name])
            raise RuntimeError(f"discord dependency inventory mismatch: missing={missing}, unexpected={unexpected}, changed={changed}")
        if not smoke(staged, proxy_url=proxy_url):
            raise RuntimeError("discord dependency installation failed its package smoke check")
        write_marker(staged, identity, runtime_tree_digest(staged))
        if os.path.lexists(target):
            info = os.lstat(target)
            if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
                raise RuntimeError("discord dependency target is not a directory")
            shutil.rmtree(target)
        os.rename(staged, target)
        directory = os.open(RUNTIME_ROOT, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.isdir(staged):
            shutil.rmtree(staged)


def prune(current, keep):
    candidates = []
    for name in os.listdir(RUNTIME_ROOT):
        path = os.path.join(RUNTIME_ROOT, name)
        if path == current or not re.fullmatch(r"[0-9a-f]{64}", name):
            continue
        try:
            info = os.lstat(path)
        except FileNotFoundError:
            continue
        if stat.S_ISDIR(info.st_mode) and not stat.S_ISLNK(info.st_mode):
            candidates.append((info.st_mtime_ns, path))
    candidates.sort(reverse=True)
    for _, path in candidates[max(0, keep - 1):]:
        shutil.rmtree(path)


def dependency_root():
    timeout_ms, keep = dependency_config()
    proxy_url = dependency_proxy_url()
    identity = dependency_identity()
    require_private_directory(VAR)
    source = os.environ.get("OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE")
    if source:
        if not os.path.isabs(source):
            raise RuntimeError("discord source dependency runtime path must be absolute")
        source = os.path.realpath(source)
        if os.path.basename(source) != identity:
            raise RuntimeError("discord source dependency runtime identity does not match the package lock")
        if not marker_matches(source, identity, proxy_url):
            raise RuntimeError("discord source dependency runtime failed its package integrity proof")
        return source
    target = os.path.join(RUNTIME_ROOT, identity)
    require_private_directory(RUNTIME_ROOT)
    descriptor = os.open(LOCK_PATH, os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0), 0o600)
    with os.fdopen(descriptor, "r+b") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        for name in os.listdir(RUNTIME_ROOT):
            if not re.fullmatch(r"\.[0-9a-f]{64}\.[0-9]+\.[0-9]+\.tmp", name):
                continue
            path = os.path.join(RUNTIME_ROOT, name)
            info = os.lstat(path)
            if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
                raise RuntimeError("discord dependency runtime has an invalid stale staging entry")
            shutil.rmtree(path)
        if not marker_matches(target, identity, proxy_url):
            install(target, identity, timeout_ms, proxy_url)
        prune(target, keep)
    return target


def main():
    target = dependency_root()
    environment = life_environment_for(target)
    os.execve(sys.executable, [sys.executable, LIFE, *sys.argv[1:]], environment)


if __name__ == "__main__":
    main()
