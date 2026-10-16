#!/usr/bin/env python3
"""Build the locked Spike executable; stdout is its absolute path (or cache key).

Ubuntu dependencies: build-essential git device-tree-compiler libgmp-dev
libmpc-dev. Uses the configure script committed upstream, without autoreconf.
Only the executable needed by difftest is installed, without pk or SDK files.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
LOCK = ROOT / "scripts/spike.lock.json"


class BuildError(Exception):
    """Build failure or an untrustworthy cache entry."""


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def capture(command: list[str], *, include_stderr: bool = False) -> str:
    try:
        result = subprocess.run(command, check=True, text=True, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT if include_stderr else subprocess.PIPE,
                                timeout=30)
        return result.stdout.strip()
    except (OSError, subprocess.SubprocessError) as error:
        raise BuildError(f"{command[0]}: {error}") from error


def build_request() -> dict:
    lock = json.loads(LOCK.read_text())
    if not re.fullmatch(r"[0-9a-f]{40}", lock["revision"]):
        raise BuildError("Spike revision must be a full 40-character commit SHA")
    # Include host ABI and compiler identities, not the changing kernel version.
    os_release = Path("/etc/os-release")
    return {
        "lock": lock,
        "recipe_sha256": digest(Path(__file__)),
        "system": platform.system(), "machine": platform.machine(),
        "libc": platform.libc_ver(),
        "os_release": os_release.read_text() if os_release.exists() else "",
        "cc_version": capture([lock["cc"], "--version"]),
        "cxx_version": capture([lock["cxx"], "--version"]),
        "linker_version": capture(["ld", "--version"]),
    }


def cache_key(request: dict) -> str:
    return hashlib.sha256(json.dumps(request, sort_keys=True).encode()).hexdigest()


def cached_binary(prefix: Path, request: dict) -> Path | None:
    if not prefix.exists():
        return None
    binary, manifest = prefix / "bin/spike", prefix / "build.json"
    try:
        saved = json.loads(manifest.read_text())
        # JSON normalizes tuples (e.g. libc version) to arrays.
        expected = json.loads(json.dumps(request))
        if saved["request"] != expected or saved["binary_sha256"] != digest(binary):
            raise BuildError(f"cache provenance/checksum mismatch: {prefix}")
        if not os.access(binary, os.X_OK):
            raise BuildError(f"cached Spike is not executable: {binary}")
    except (OSError, ValueError, KeyError) as error:
        raise BuildError(f"incomplete Spike cache: {prefix}: {error}") from error
    return binary


def build(request: dict, cache: Path, jobs: int) -> Path:
    key = cache_key(request)
    prefix = cache / key
    ready = cached_binary(prefix, request)
    if ready:
        print(f"spike-build: verified cache {key[:12]}", file=sys.stderr)
        return ready
    cache.mkdir(parents=True, exist_ok=True)
    log_path = cache / f"{key}.log"
    print(f"spike-build: building {request['lock']['revision']} ({jobs} jobs)", file=sys.stderr)
    print(f"spike-build: log {log_path}", file=sys.stderr)
    lock = request["lock"]
    env = dict(os.environ, CC=lock["cc"], CXX=lock["cxx"],
               CFLAGS=lock["cflags"], CXXFLAGS=lock["cxxflags"],
               CPPFLAGS="", LDFLAGS="", CONFIG_SITE="/dev/null")
    with tempfile.TemporaryDirectory(prefix=".build-", dir=cache) as temporary:
        work = Path(temporary)
        source, build_dir, staged = work / "source", work / "build", work / "install"
        build_dir.mkdir()
        with log_path.open("w") as log:
            def run(command: list[str], cwd: Path, timeout: int) -> None:
                print("$ " + " ".join(command), file=log, flush=True)
                try:
                    subprocess.run(command, cwd=cwd, env=env, stdout=log,
                                   stderr=subprocess.STDOUT, check=True, timeout=timeout)
                except (OSError, subprocess.SubprocessError) as error:
                    raise BuildError(f"{command[0]} failed: {error}; see {log_path}") from error

            run(["git", "init", "--quiet", str(source)], work, 30)
            run(["git", "-C", str(source), "fetch", "--depth=1", "--no-tags",
                 lock["repository"], lock["revision"]], work, 180)
            run(["git", "-C", str(source), "checkout", "--quiet", "--detach",
                 lock["revision"]], work, 30)
            actual = capture(["git", "-C", str(source), "rev-parse", "HEAD"])
            if actual != lock["revision"]:
                raise BuildError(f"checkout differs from lock: {actual}")
            run([str(source / "configure"), f"--prefix={prefix}", *lock["configure"]],
                build_dir, 180)
            run(["make", f"-j{jobs}", lock["target"]], build_dir, 1800)
        (staged / "bin").mkdir(parents=True)
        binary = staged / "bin/spike"
        shutil.copy2(build_dir / "spike", binary)
        help_text = capture([str(binary), "--help"], include_stderr=True)
        if not all(option in help_text for option in ("--debug-cmd", "--log-commits")):
            raise BuildError("built Spike lacks debugger/commit logging support")
        (staged / "build.json").write_text(json.dumps({
            "request": request, "binary_sha256": digest(binary),
        }, indent=2, sort_keys=True) + "\n")
        # Publish only complete builds. Temporary source/object files are removed
        # on success or failure; the build log stays beside the cache entry.
        staged.rename(prefix)
    return cached_binary(prefix, request)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache-dir", type=Path, default=ROOT / "target/tools/spike")
    parser.add_argument("--jobs", type=int, default=min(os.cpu_count() or 2, 4))
    parser.add_argument("--cache-key", action="store_true", help="print key without fetching/building")
    args = parser.parse_args(argv)
    if args.jobs < 1:
        parser.error("jobs must be positive")
    try:
        request = build_request()
        result = cache_key(request) if args.cache_key else build(request, args.cache_dir.resolve(), args.jobs)
        print(result)
        return 0
    except (BuildError, OSError, ValueError, KeyError) as error:
        print(f"FAIL: spike-build: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
