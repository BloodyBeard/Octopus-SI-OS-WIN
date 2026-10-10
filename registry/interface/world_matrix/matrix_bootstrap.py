#!/usr/bin/env python3
import base64
import csv
import fcntl
import hashlib
import hmac
import importlib.metadata
import io
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

from matrix_contract import validate_config
from matrix_contract import validate_proxy_url


PACKAGE_DIR = os.path.dirname(os.path.abspath(__file__))
INTERFACE_DIR = os.path.abspath(os.environ["OCTOPUS_INTERFACE_DIR"])
VAR_DIR = os.path.abspath(os.environ["OCTOPUS_VAR_DIR"])
STAGING_ROOT = os.path.join(VAR_DIR, "python-environment-staging")
LOCK_PATH = os.path.join(PACKAGE_DIR, "dependency_lock.json")
CONFIG_PATH = os.path.join(INTERFACE_DIR, "config.json")
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


def require_object(value, name):
    if not isinstance(value, dict):
        raise ValueError(f"{name} must be an object")
    return value


def require_keys(value, required, name):
    value = require_object(value, name)
    missing = sorted(set(required) - set(value))
    unknown = sorted(set(value) - set(required))
    if missing:
        raise ValueError(f"{name} is missing keys: {', '.join(missing)}")
    if unknown:
        raise ValueError(f"{name} has unknown keys: {', '.join(unknown)}")
    return value


def private_directory(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    current = os.lstat(path)
    if stat.S_ISLNK(current.st_mode) or not stat.S_ISDIR(current.st_mode) or current.st_uid != os.getuid():
        raise PermissionError(f"Matrix dependency directory is not private: {path}")
    if stat.S_IMODE(current.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


def private_json(path, value):
    parent = private_directory(os.path.dirname(path))
    temporary = os.path.join(parent, f".{os.path.basename(path)}.{os.getpid()}.{time.time_ns()}.tmp")
    descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            descriptor = None
            json.dump(value, handle, ensure_ascii=False, sort_keys=True, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o600)
        directory = os.open(parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if descriptor is not None:
            os.close(descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def read_json(path, name):
    with open(path, "r", encoding="utf-8") as handle:
        return require_object(json.load(handle), name)


def crypto_mode(config):
    crypto = require_object(config.get("crypto"), "Matrix crypto config")
    value = os.environ.get(crypto["mode_env"], crypto["mode"])
    if not isinstance(value, str) or not value or value.strip() != value:
        raise ValueError("Matrix E2EE mode is invalid")
    value = value.lower()
    aliases = {"require": "required", "true": "required", "1": "required", "yes": "required", "on": "required", "prefer": "optional", "preferred": "optional", "false": "off", "0": "off", "no": "off"}
    value = aliases.get(value, value)
    if value not in {"off", "optional", "required"}:
        raise ValueError("Matrix E2EE mode is invalid")
    return value


def canonical_distribution_name(value):
    if not isinstance(value, str) or not value:
        raise ValueError("Matrix distribution name is malformed")
    return re.sub(r"[-_.]+", "-", value).lower()


def selected_profile(lock, mode):
    rows = lock.get("requirements")
    if not isinstance(rows, list) or not rows:
        raise ValueError("Matrix dependency lock has no source requirements")
    source_paths = set()
    for index, row in enumerate(rows):
        row = require_keys(row, {"path", "sha256", "modes"}, f"Matrix dependency requirement {index}")
        path = row.get("path")
        digest = row.get("sha256")
        modes = row.get("modes")
        if not isinstance(path, str) or path != os.path.basename(path) or not re.fullmatch(r"[0-9a-f]{64}", str(digest)) or not isinstance(modes, list) or not modes or len(set(modes)) != len(modes) or any(item not in {"off", "optional", "required"} for item in modes):
            raise ValueError("Matrix dependency requirement is malformed")
        if path in source_paths:
            raise ValueError("Matrix dependency requirement path is duplicated")
        source_paths.add(path)
        full_path = os.path.join(PACKAGE_DIR, path)
        with open(full_path, "rb") as handle:
            observed = hashlib.sha256(handle.read()).hexdigest()
        if observed != digest:
            raise RuntimeError(f"Matrix dependency requirement {path} failed its pinned digest")
    profiles = lock.get("resolved_profiles")
    if not isinstance(profiles, list) or not profiles:
        raise ValueError("Matrix dependency lock has no resolved profiles")
    target = (platform.system().lower(), platform.machine().lower(), f"{sys.version_info.major}.{sys.version_info.minor}")
    matches = []
    profile_ids = set()
    profile_targets = set()
    for index, profile in enumerate(profiles):
        profile = require_keys(profile, {"id", "platform", "machine", "python", "modes", "requirements_path", "sha256", "distributions", "smoke_imports"}, f"Matrix dependency profile {index}")
        for key in ("id", "platform", "machine", "python", "requirements_path", "sha256"):
            if not isinstance(profile[key], str) or not profile[key]:
                raise ValueError(f"Matrix dependency profile {index}.{key} is malformed")
        if profile["requirements_path"] != os.path.basename(profile["requirements_path"]) or not re.fullmatch(r"[0-9a-f]{64}", profile["sha256"]):
            raise ValueError(f"Matrix dependency profile {index} requirement identity is malformed")
        if profile["id"] in profile_ids:
            raise ValueError("Matrix dependency profile id is duplicated")
        profile_ids.add(profile["id"])
        if not isinstance(profile["modes"], list) or not profile["modes"] or len(set(profile["modes"])) != len(profile["modes"]) or any(item not in {"off", "optional", "required"} for item in profile["modes"]):
            raise ValueError(f"Matrix dependency profile {index}.modes is malformed")
        for item in profile["modes"]:
            target_key = (profile["platform"].lower(), profile["machine"].lower(), profile["python"], item)
            if target_key in profile_targets:
                raise ValueError("Matrix dependency profiles overlap one target and mode")
            profile_targets.add(target_key)
        distributions = require_object(profile["distributions"], f"Matrix dependency profile {index}.distributions")
        if not distributions:
            raise ValueError(f"Matrix dependency profile {index} has no distributions")
        canonical_names = set()
        for name, version in distributions.items():
            if not isinstance(name, str) or not name or not isinstance(version, str) or not version:
                raise ValueError(f"Matrix dependency profile {index} distribution is malformed")
            canonical = canonical_distribution_name(name)
            if canonical in canonical_names:
                raise ValueError(f"Matrix dependency profile {index} repeats a distribution identity")
            canonical_names.add(canonical)
        smoke_imports = profile["smoke_imports"]
        if not isinstance(smoke_imports, list) or not smoke_imports or len(set(smoke_imports)) != len(smoke_imports) or any(not isinstance(name, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.]*", name) for name in smoke_imports):
            raise ValueError(f"Matrix dependency profile {index}.smoke_imports is malformed")
        if (profile["platform"].lower(), profile["machine"].lower(), profile["python"]) == target and mode in profile["modes"]:
            matches.append(profile)
    if len(matches) != 1:
        raise RuntimeError(f"Matrix has no unique fully locked dependency profile for {target[0]}/{target[1]}/python-{target[2]}/{mode}")
    profile = matches[0]
    requirements_path = os.path.join(PACKAGE_DIR, profile["requirements_path"])
    with open(requirements_path, "rb") as handle:
        observed = hashlib.sha256(handle.read()).hexdigest()
    if observed != profile["sha256"]:
        raise RuntimeError(f"Matrix resolved requirement {profile['requirements_path']} failed its pinned digest")
    return profile, [requirements_path]


def lock_digest(lock, mode, profile):
    body = {"lock": lock, "mode": mode, "profile": profile["id"], "python": [sys.version_info.major, sys.version_info.minor]}
    return hashlib.sha256(json.dumps(body, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()


def distribution_integrity(distribution, venv_path):
    record = distribution.read_text("RECORD")
    name = distribution.metadata.get("Name") or "unknown"
    if not isinstance(record, str) or not record:
        raise RuntimeError(f"Matrix managed distribution {name} has no RECORD integrity manifest")
    root = os.path.realpath(venv_path)
    seen = set()
    owned = set()
    unhashed = []
    material = hashlib.sha256()
    rows = list(csv.reader(io.StringIO(record)))
    if not rows:
        raise RuntimeError(f"Matrix managed distribution {name} has an empty RECORD integrity manifest")
    for index, row in enumerate(rows):
        if len(row) != 3 or not row[0] or row[0] in seen:
            raise RuntimeError(f"Matrix managed distribution {name} has malformed RECORD row {index}")
        relative, encoded_hash, encoded_size = row
        seen.add(relative)
        candidate = os.path.abspath(os.fspath(distribution.locate_file(relative)))
        resolved_candidate = os.path.realpath(candidate)
        if os.path.commonpath((resolved_candidate, root)) != root:
            raise RuntimeError(f"Matrix managed distribution {name} RECORD escapes its private environment")
        normalized_relative = relative.replace("\\", "/")
        if not encoded_hash and "/__pycache__/" in "/" + normalized_relative and normalized_relative.endswith(".pyc"):
            if os.path.lexists(candidate):
                raise RuntimeError(f"Matrix managed distribution {name} contains unhashed bytecode: {relative}")
            material.update(relative.encode("utf-8"))
            material.update(b"\0absent-unhashed-bytecode\n")
            continue
        state = os.lstat(candidate)
        if stat.S_ISLNK(state.st_mode) or not stat.S_ISREG(state.st_mode):
            raise RuntimeError(f"Matrix managed distribution {name} RECORD targets a non-regular file")
        owned.add(candidate)
        if encoded_size:
            try:
                expected_size = int(encoded_size)
            except ValueError as error:
                raise RuntimeError(f"Matrix managed distribution {name} RECORD has invalid size") from error
            if expected_size != state.st_size:
                raise RuntimeError(f"Matrix managed distribution {name} file size drifted: {relative}")
        digest = hashlib.sha256()
        with open(candidate, "rb") as handle:
            while True:
                chunk = handle.read(1048576)
                if not chunk:
                    break
                digest.update(chunk)
        observed = digest.digest()
        if encoded_hash:
            algorithm, separator, value = encoded_hash.partition("=")
            if separator != "=" or algorithm != "sha256" or not value:
                raise RuntimeError(f"Matrix managed distribution {name} RECORD uses an unsupported digest")
            try:
                expected = base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))
            except Exception as error:
                raise RuntimeError(f"Matrix managed distribution {name} RECORD digest is malformed") from error
            if not hmac.compare_digest(expected, observed):
                raise RuntimeError(f"Matrix managed distribution {name} file digest drifted: {relative}")
        else:
            unhashed.append(relative)
        material.update(relative.encode("utf-8"))
        material.update(b"\0")
        material.update(observed)
        material.update(b"\0")
        material.update(str(state.st_size).encode("ascii"))
        material.update(b"\n")
    if len(unhashed) != 1 or not unhashed[0].endswith(".dist-info/RECORD"):
        raise RuntimeError(f"Matrix managed distribution {name} has unexpected unhashed RECORD entries")
    return {
        "record_sha256": hashlib.sha256(record.encode("utf-8")).hexdigest(),
        "content_sha256": material.hexdigest(),
        "record_file_count": len(rows),
    }, owned


def inventory(venv_path):
    site_packages = os.path.join(venv_path, "lib", f"python{sys.version_info.major}.{sys.version_info.minor}", "site-packages")
    if not os.path.isdir(site_packages):
        raise RuntimeError("Matrix managed environment has no site-packages directory")
    rows = {}
    owned = set()
    for distribution in importlib.metadata.distributions(path=[site_packages]):
        name = distribution.metadata.get("Name")
        if not isinstance(name, str) or not name:
            raise RuntimeError("Matrix managed environment contains an unnamed distribution")
        canonical = canonical_distribution_name(name)
        if canonical in rows:
            raise RuntimeError(f"Matrix managed environment repeats distribution {canonical}")
        evidence, distribution_files = distribution_integrity(distribution, venv_path)
        rows[canonical] = {"version": distribution.version, **evidence}
        owned.update(distribution_files)
    for root, directories, files in os.walk(site_packages):
        for name in directories:
            state = os.lstat(os.path.join(root, name))
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
                raise RuntimeError("Matrix managed environment contains an invalid site-packages directory")
        for name in files:
            path = os.path.abspath(os.path.join(root, name))
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISREG(state.st_mode) or path not in owned:
                raise RuntimeError(f"Matrix managed environment contains an unowned site-packages file: {os.path.relpath(path, site_packages)}")
    return rows


def verify_inventory(profile, rows):
    required = {canonical_distribution_name(name): version for name, version in profile["distributions"].items()}
    if set(rows) != set(required):
        missing = sorted(set(required) - set(rows))
        unexpected = sorted(set(rows) - set(required))
        raise RuntimeError(f"Matrix managed environment inventory drifted: missing={missing}, unexpected={unexpected}")
    for name, version in required.items():
        row = rows.get(name)
        if not isinstance(row, dict) or row.get("version") != version or not re.fullmatch(r"[0-9a-f]{64}", str(row.get("record_sha256"))) or not re.fullmatch(r"[0-9a-f]{64}", str(row.get("content_sha256"))) or isinstance(row.get("record_file_count"), bool) or not isinstance(row.get("record_file_count"), int) or row["record_file_count"] < 1:
            raise RuntimeError(f"Matrix managed environment does not contain pinned {name}=={version}")


def dependency_process_environment(proxy=""):
    environment = {
        name: os.environ[name]
        for name in DEPENDENCY_ENV_ALLOWLIST
        if os.environ.get(name)
    }
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    environment["PIP_DISABLE_PIP_VERSION_CHECK"] = "1"
    environment["PIP_CONFIG_FILE"] = os.devnull
    if proxy:
        proxy_scheme = urllib.parse.urlsplit(proxy).scheme
        if proxy_scheme in {"http", "https"}:
            environment["HTTP_PROXY"] = proxy
            environment["HTTPS_PROXY"] = proxy
        else:
            environment["ALL_PROXY"] = proxy
    return environment


def smoke_environment(venv_path, profile):
    python = os.path.join(venv_path, "bin", "python")
    command = [python, "-I", "-B", "-c", ";".join(f"import {name}" for name in profile["smoke_imports"])]
    result = subprocess.run(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=30,
        env=dependency_process_environment(),
    )
    if result.returncode != 0:
        raise RuntimeError(f"Matrix managed environment smoke import failed with exit status {result.returncode}")


def remove_unhashed_bytecode(venv_path):
    for root, directories, files in os.walk(venv_path, topdown=False):
        for name in files:
            if not name.endswith(".pyc"):
                continue
            path = os.path.join(root, name)
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISREG(state.st_mode):
                raise RuntimeError("Matrix managed environment bytecode path is not a regular file")
            os.unlink(path)
        for name in directories:
            if name != "__pycache__":
                continue
            path = os.path.join(root, name)
            state = os.lstat(path)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
                raise RuntimeError("Matrix managed environment bytecode directory is invalid")
            if not os.listdir(path):
                os.rmdir(path)


def environment_current(venv_path, evidence_path, digest, profile, mode):
    try:
        evidence = read_json(evidence_path, "Matrix dependency evidence")
        if evidence.get("schema_version") != 1 or evidence.get("lock_sha256") != digest or evidence.get("profile") != profile["id"] or evidence.get("mode") != mode:
            return False
        rows = inventory(venv_path)
        verify_inventory(profile, rows)
        smoke_environment(venv_path, profile)
        return rows == evidence.get("distributions")
    except (FileNotFoundError, OSError, ValueError, RuntimeError, subprocess.SubprocessError, json.JSONDecodeError):
        return False


def install_environment(venv_path, evidence_path, digest, lock, profile, mode, requirements):
    private_directory(STAGING_ROOT)
    staging = os.path.join(STAGING_ROOT, f"{os.getpid()}.{time.time_ns()}")
    shutil.rmtree(staging, ignore_errors=True)
    try:
        bootstrap = subprocess.run(
            [sys.executable, "-I", "-m", "venv", "--copies", staging],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=lock["install_timeout_seconds"],
            env=dependency_process_environment(),
        )
        if bootstrap.returncode != 0:
            raise RuntimeError(f"Matrix dependency environment creation failed with exit status {bootstrap.returncode}")
        python = os.path.join(staging, "bin", "python")
        command = [python, "-I", "-m", "pip", "install", "--isolated", "--disable-pip-version-check", "--no-input", "--only-binary=:all:", "--require-hashes"]
        for path in requirements:
            command.extend(["--requirement", path])
        proxy = validate_proxy_url(os.environ.get("MATRIX_PROXY", ""))
        child_env = dependency_process_environment(proxy)
        result = subprocess.run(
            command,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=lock["install_timeout_seconds"],
            env=child_env,
        )
        if result.returncode != 0:
            raise RuntimeError(f"Matrix dependency installation failed with exit status {result.returncode}")
        cleanup = subprocess.run(
            [python, "-I", "-m", "pip", "uninstall", "--yes", "pip", "setuptools"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=lock["install_timeout_seconds"],
            env=child_env,
        )
        if cleanup.returncode != 0:
            raise RuntimeError(f"Matrix dependency installer cleanup failed with exit status {cleanup.returncode}")
        remove_unhashed_bytecode(staging)
        rows = inventory(staging)
        verify_inventory(profile, rows)
        smoke_environment(staging, profile)
        shutil.rmtree(venv_path, ignore_errors=True)
        os.replace(staging, venv_path)
        private_json(evidence_path, {"schema_version": 1, "lock_sha256": digest, "profile": profile["id"], "mode": mode, "python": sys.version.split()[0], "distributions": rows})
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def sanitize(value):
    if not isinstance(value, str):
        value = str(value)
    for env_name in ("MATRIX_ACCESS_TOKEN", "MATRIX_PASSWORD", "MATRIX_PROXY", "MATRIX_RECOVERY_KEY"):
        token = os.environ.get(env_name, "")
        if len(token) >= 4:
            for variant in (token, urllib.parse.quote(token, safe=""), urllib.parse.quote_plus(token, safe=""), base64.b64encode(token.encode("utf-8")).decode("ascii")):
                value = value.replace(variant, "[secret-redacted]")
    return re.sub(r"((?:https?|socks(?:4|5h?)?)://)([^/@\s]+)@", r"\1[credentials-redacted]@", value)


def main():
    os.umask(0o077)
    private_directory(VAR_DIR)
    lock = require_keys(
        read_json(LOCK_PATH, "Matrix dependency lock"),
        {"schema_version", "managed_venv_subdir", "evidence_file", "install_timeout_seconds", "requirements", "resolved_profiles"},
        "Matrix dependency lock",
    )
    if lock.get("schema_version") != 1:
        raise ValueError("Matrix dependency lock schema is unsupported")
    if isinstance(lock["install_timeout_seconds"], bool) or not isinstance(lock["install_timeout_seconds"], int) or not 60 <= lock["install_timeout_seconds"] <= 3600:
        raise ValueError("Matrix dependency install timeout must be 60..3600 seconds")
    config = validate_config(read_json(CONFIG_PATH, "Matrix config"), os.environ["OCTOPUS_INTERFACE_ID"])
    mode = crypto_mode(config)
    profile, requirements = selected_profile(lock, mode)
    digest = lock_digest(lock, mode, profile)
    source_runtime = os.environ.get("OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE")
    source_evidence = os.environ.get("OCTOPUS_MATRIX_DEPENDENCY_EVIDENCE_SOURCE")
    if bool(source_runtime) != bool(source_evidence):
        raise RuntimeError("Matrix source dependency runtime and evidence must be declared together")
    if source_runtime:
        if not os.path.isabs(source_runtime) or not os.path.isabs(source_evidence):
            raise RuntimeError("Matrix source dependency paths must be absolute")
        venv_path = os.path.realpath(source_runtime)
        evidence_path = os.path.realpath(source_evidence)
        if not environment_current(venv_path, evidence_path, digest, profile, mode):
            raise RuntimeError("Matrix source dependency runtime failed its lock and RECORD proof")
        python = os.path.join(venv_path, "bin", "python")
        environment = dict(os.environ)
        environment["OCTOPUS_CONTENT_HASH_RUNTIME"] = venv_path
        os.execve(python, [python, "-B", os.path.join(PACKAGE_DIR, "matrix_life.py"), *sys.argv[1:]], environment)
    venv_path = os.path.join(VAR_DIR, lock.get("managed_venv_subdir", ""))
    evidence_path = os.path.join(VAR_DIR, lock.get("evidence_file", ""))
    if os.path.dirname(venv_path) != VAR_DIR or os.path.dirname(evidence_path) != VAR_DIR:
        raise ValueError("Matrix dependency lock paths must stay in OCTOPUS_VAR_DIR")
    lock_file = os.path.join(VAR_DIR, "dependency-bootstrap.lock")
    descriptor = os.open(lock_file, os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0), 0o600)
    with os.fdopen(descriptor, "r+b") as handle:
        lock_stat = os.fstat(handle.fileno())
        if not stat.S_ISREG(lock_stat.st_mode) or lock_stat.st_uid != os.getuid():
            raise PermissionError("Matrix dependency bootstrap lock is not an owner-controlled regular file")
        if stat.S_IMODE(lock_stat.st_mode) != 0o600:
            os.fchmod(handle.fileno(), 0o600)
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        if not environment_current(venv_path, evidence_path, digest, profile, mode):
            install_environment(venv_path, evidence_path, digest, lock, profile, mode, requirements)
    python = os.path.join(venv_path, "bin", "python")
    environment = dict(os.environ)
    environment["OCTOPUS_CONTENT_HASH_RUNTIME"] = venv_path
    os.execve(python, [python, "-B", os.path.join(PACKAGE_DIR, "matrix_life.py"), *sys.argv[1:]], environment)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        sys.stderr.write(sanitize(f"Matrix dependency bootstrap failed: {error}") + "\n")
        raise SystemExit(1)
