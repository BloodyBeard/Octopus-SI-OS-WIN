#!/usr/bin/env python3
import fcntl
import hashlib
import importlib.metadata
import json
import os
import platform
import re
import shutil
import stat
import subprocess
import sys
import time


ROOT = os.path.dirname(os.path.abspath(__file__))
VAR_DIR = os.path.abspath(os.environ["OCTOPUS_VAR_DIR"])
LOCK_PATH = os.path.join(ROOT, "requirements-content-hash.lock")
RUNTIME_ROOT = os.path.join(VAR_DIR, "content-hash-python-runtime")
BOOT_LOCK_PATH = os.path.join(VAR_DIR, "content-hash-python-runtime.lock")
EXPECTED_INVENTORY = {"blake3": "1.0.9"}
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


def private_directory(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    current = os.lstat(path)
    if stat.S_ISLNK(current.st_mode) or not stat.S_ISDIR(current.st_mode):
        raise PermissionError(f"content-hash dependency path is not a directory: {path}")
    if hasattr(os, "geteuid") and current.st_uid != os.geteuid():
        raise PermissionError(f"content-hash dependency path has the wrong owner: {path}")
    if stat.S_IMODE(current.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


def read_json(path):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    with os.fdopen(descriptor, "r", encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise RuntimeError(f"{path} must contain a JSON object")
    return value


def identity():
    digest = hashlib.sha256()
    descriptor = os.open(LOCK_PATH, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    with os.fdopen(descriptor, "rb") as handle:
        digest.update(handle.read())
    digest.update(sys.implementation.name.encode("ascii"))
    digest.update(f"{sys.version_info.major}.{sys.version_info.minor}".encode("ascii"))
    digest.update(sys.platform.encode("ascii"))
    digest.update(platform.machine().encode("ascii"))
    return digest.hexdigest()


def canonical_distribution_name(value):
    return re.sub(r"[-_.]+", "-", value).lower()


def installed_inventory(target):
    result = {}
    for distribution in importlib.metadata.distributions(path=[target]):
        raw_name = distribution.metadata.get("Name")
        if not isinstance(raw_name, str) or not raw_name:
            raise RuntimeError("content-hash runtime contains an unnamed distribution")
        name = canonical_distribution_name(raw_name)
        if name in result:
            raise RuntimeError(f"content-hash runtime repeats distribution {name}")
        result[name] = distribution.version
    return result


def normalize_tree_permissions(target):
    for current, directories, files in os.walk(target, topdown=True, followlinks=False):
        state = os.lstat(current)
        if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
            raise PermissionError("content-hash runtime contains a non-directory tree node")
        os.chmod(current, 0o700)
        for name in directories:
            path = os.path.join(current, name)
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
                raise PermissionError("content-hash runtime contains an invalid directory")
        for name in files:
            path = os.path.join(current, name)
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISREG(state.st_mode):
                raise PermissionError("content-hash runtime contains an invalid file")
            os.chmod(path, 0o600)


def runtime_tree_digest(target):
    digest = hashlib.sha256()
    marker_name = ".octopus-content-hash-runtime.json"
    for current, directories, files in os.walk(target, topdown=True, followlinks=False):
        directories.sort()
        files.sort()
        current_state = os.lstat(current)
        if (
            stat.S_ISLNK(current_state.st_mode)
            or not stat.S_ISDIR(current_state.st_mode)
            or stat.S_IMODE(current_state.st_mode) != 0o700
        ):
            raise PermissionError("content-hash runtime directory integrity failed")
        relative_current = os.path.relpath(current, target)
        digest.update(b"D\0")
        digest.update(relative_current.encode("utf-8"))
        digest.update(b"\n")
        for name in directories:
            path = os.path.join(current, name)
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
                raise PermissionError("content-hash runtime directory integrity failed")
        for name in files:
            if relative_current == "." and name == marker_name:
                continue
            path = os.path.join(current, name)
            state = os.lstat(path)
            if (
                stat.S_ISLNK(state.st_mode)
                or not stat.S_ISREG(state.st_mode)
                or stat.S_IMODE(state.st_mode) != 0o600
            ):
                raise PermissionError("content-hash runtime file integrity failed")
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


def dependency_environment():
    environment = {
        name: os.environ[name]
        for name in DEPENDENCY_ENV_ALLOWLIST
        if os.environ.get(name)
    }
    environment["PIP_CONFIG_FILE"] = os.devnull
    environment["PIP_DISABLE_PIP_VERSION_CHECK"] = "1"
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    return environment


def smoke(target):
    code = (
        "import os,sys;root=os.path.realpath(sys.argv[1]);sys.path.insert(0,root);"
        "import blake3;path=os.path.realpath(blake3.__file__);"
        "assert os.path.commonpath((path,root))==root;"
        "assert blake3.__version__=='1.0.9';"
        "assert blake3.blake3(b'').hexdigest()=='af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262';"
        "assert blake3.blake3(b'abc').hexdigest()=='6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85'"
    )
    result = subprocess.run(
        [sys.executable, "-I", "-B", "-c", code, target],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=30,
        check=False,
        env=dependency_environment(),
    )
    return result.returncode == 0


def marker_matches(target, expected_identity):
    try:
        state = os.lstat(target)
        if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
            return False
        if stat.S_IMODE(state.st_mode) != 0o700:
            return False
        marker_path = os.path.join(target, ".octopus-content-hash-runtime.json")
        marker_state = os.lstat(marker_path)
        if stat.S_ISLNK(marker_state.st_mode) or not stat.S_ISREG(marker_state.st_mode):
            return False
        if stat.S_IMODE(marker_state.st_mode) != 0o600:
            return False
        marker = read_json(marker_path)
        return (
            set(marker) == {"schema_version", "identity", "tree_sha256"}
            and marker["schema_version"] == 1
            and marker["identity"] == expected_identity
            and isinstance(marker["tree_sha256"], str)
            and re.fullmatch(r"[0-9a-f]{64}", marker["tree_sha256"]) is not None
            and runtime_tree_digest(target) == marker["tree_sha256"]
            and installed_inventory(target) == EXPECTED_INVENTORY
            and smoke(target)
        )
    except (FileNotFoundError, OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        return False


def write_marker(target, expected_identity, tree_sha256):
    path = os.path.join(target, ".octopus-content-hash-runtime.json")
    descriptor = os.open(
        path,
        os.O_CREAT | os.O_EXCL | os.O_WRONLY | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
        json.dump(
            {
                "schema_version": 1,
                "identity": expected_identity,
                "tree_sha256": tree_sha256,
            },
            handle,
            sort_keys=True,
        )
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())


def install(target, expected_identity):
    staged = os.path.join(
        RUNTIME_ROOT,
        f".{expected_identity}.{os.getpid()}.{time.time_ns()}.staging",
    )
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
                LOCK_PATH,
                "--require-hashes",
                "--only-binary=:all:",
                "--no-deps",
                "--no-input",
                "--no-compile",
                "--no-cache-dir",
                "--no-warn-script-location",
            ],
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=600,
            check=False,
            env=dependency_environment(),
        )
        if result.returncode != 0:
            raise RuntimeError(
                f"content-hash dependency installation failed with exit {result.returncode}"
            )
        normalize_tree_permissions(staged)
        if installed_inventory(staged) != EXPECTED_INVENTORY:
            raise RuntimeError("content-hash dependency inventory does not match its lock")
        if not smoke(staged):
            raise RuntimeError("content-hash dependency failed its native smoke check")
        tree_sha256 = runtime_tree_digest(staged)
        write_marker(staged, expected_identity, tree_sha256)
        if os.path.lexists(target):
            current = os.lstat(target)
            if stat.S_ISLNK(current.st_mode) or not stat.S_ISDIR(current.st_mode):
                raise PermissionError("content-hash runtime target is not a directory")
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


def dependency_root():
    source = os.environ.get("OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE")
    if source:
        if not os.path.isabs(source):
            raise RuntimeError("content-hash source runtime path must be absolute")
        source = os.path.realpath(source)
        expected_identity = identity()
        if os.path.basename(source) != expected_identity:
            raise RuntimeError("content-hash source runtime identity does not match this package lock")
        if not marker_matches(source, expected_identity):
            raise RuntimeError("content-hash source runtime failed its package integrity proof")
        return source
    private_directory(VAR_DIR)
    private_directory(RUNTIME_ROOT)
    expected_identity = identity()
    target = os.path.join(RUNTIME_ROOT, expected_identity)
    descriptor = os.open(
        BOOT_LOCK_PATH,
        os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    with os.fdopen(descriptor, "r+b") as handle:
        current = os.fstat(handle.fileno())
        if not stat.S_ISREG(current.st_mode):
            raise PermissionError("content-hash bootstrap lock is not a regular file")
        if hasattr(os, "geteuid") and current.st_uid != os.geteuid():
            raise PermissionError("content-hash bootstrap lock has the wrong owner")
        if stat.S_IMODE(current.st_mode) != 0o600:
            os.fchmod(handle.fileno(), 0o600)
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        for name in os.listdir(RUNTIME_ROOT):
            if not re.fullmatch(r"\.[0-9a-f]{64}\.[0-9]+\.[0-9]+\.staging", name):
                continue
            path = os.path.join(RUNTIME_ROOT, name)
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
                raise PermissionError("content-hash runtime has an invalid stale staging entry")
            shutil.rmtree(path)
        if not marker_matches(target, expected_identity):
            install(target, expected_identity)
        for name in os.listdir(RUNTIME_ROOT):
            path = os.path.join(RUNTIME_ROOT, name)
            if path == target or not re.fullmatch(r"[0-9a-f]{64}", name):
                continue
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
                raise PermissionError("content-hash runtime root contains an invalid entry")
            shutil.rmtree(path)
    return target


def main():
    if len(sys.argv) < 2:
        raise RuntimeError("content-hash bootstrap requires a package life path")
    life_path = os.path.realpath(sys.argv[1])
    if os.path.commonpath((life_path, ROOT)) != ROOT or not os.path.isfile(life_path):
        raise RuntimeError("content-hash bootstrap life path escapes its package")
    target = dependency_root()
    environment = dict(os.environ)
    environment["PYTHONPATH"] = target
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    environment["OCTOPUS_CONTENT_HASH_RUNTIME"] = target
    os.execve(sys.executable, [sys.executable, "-B", life_path, *sys.argv[2:]], environment)


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except Exception as error:
        sys.stderr.write(f"content-hash dependency bootstrap failed: {error}\n")
        raise SystemExit(1)
