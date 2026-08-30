"""Getting a solc binary that runs on *this* machine.

py-solc-x downloads from `binaries.soliditylang.org/{linux,macosx,windows}-amd64` and
detects the OS only — there is no architecture check anywhere in it. On arm64 Linux or
Apple silicon it therefore fetches an x86-64 binary and dies validating it, which is why
sevm ran on x86_64 alone. Provisioning happens here instead; py-solc-x still *invokes*
solc (`compile_standard(solc_binary=...)`), which is the part of it that works.

The platform map follows svm, Foundry's version manager, because it is the one
implementation that has kept up with where arm64 builds actually live:

  linux x86_64    official linux-amd64
  linux arm64     nikitastupin/solc below 0.8.31, official linux-arm64 from 0.8.31
  macOS x86_64    official macosx-amd64
  macOS arm64     alloy-rs/solc-builds for 0.8.5-0.8.24, official macosx-amd64 either
                  side of it (universal binaries since 0.8.24, Rosetta below 0.8.5)
  windows         official windows-amd64, which arm64 Windows runs under emulation

The third-party repositories are pinned to the same commits svm pins, and every download
is checked against the sha256 its release list publishes, so a moved branch or a swapped
file fails closed.

A binary is confirmed by running it, never by having downloaded it: `solc-static-linux`
is dynamically linked (it wants `/lib64/ld-linux-x86-64.so.2` and glibc 2.14), so on musl
or NixOS the file arrives fine and cannot execute. That check is what a wasm fallback
would hang off later.
"""

from __future__ import annotations

import functools
import hashlib
import os
import platform
import re
import shutil
import subprocess
import sys
import zipfile
from dataclasses import dataclass
from io import BytesIO

import requests
import solcx
from packaging.version import InvalidVersion, Version

from .. import cache
from . import wasm
from .model import CompileError

_OFFICIAL = "https://binaries.soliditylang.org"

# Pinned exactly as svm pins them; both publish `{builds: [{version, sha256}], releases:
# {version: filename}}` rather than the official schema.
_LINUX_ARM64_BUILDS = (
    "https://raw.githubusercontent.com/nikitastupin/solc"
    "/2287d4326237172acf91ce42fd7ec18a67b7f512/linux/aarch64"
)
_MACOS_ARM64_BUILDS = (
    "https://raw.githubusercontent.com/alloy-rs/solc-builds"
    "/e4b80d33bc4d015b2fc3583e217fbf248b2014e1/macosx/aarch64"
)

# Solidity's own arm64 Linux builds start here; below it only the third-party ones exist.
_LINUX_ARM64_OFFICIAL = Version("0.8.31")
# macOS binaries are universal from 0.8.24, and native arm64 third-party builds start at
# 0.8.5; between the two the official x86-64 build needs Rosetta, so prefer the native one.
_MACOS_ARM64_NATIVE = Version("0.8.5")
_MACOS_UNIVERSAL = Version("0.8.24")

_TIMEOUT = 30
_HEADERS = {"User-Agent": "sevm"}

# Set by `--solc-binary`, and read before anything is looked up or downloaded: on a
# platform with no published build, a compiler the user already has is the whole answer.
_override: str | None = None


@dataclass(frozen=True)
class Compiler:
    """A solc that runs here: a native binary, or soljson.js under a JS runtime."""

    version: str
    path: str
    wasm: bool = False

    def compile(self, payload: dict) -> dict:
        if self.wasm:
            return wasm.compile_standard(self.path, payload)
        try:
            return solcx.compile_standard(payload, solc_binary=self.path)
        except solcx.exceptions.SolcError as exc:
            raise CompileError(str(exc)) from exc


@dataclass(frozen=True)
class Release:
    """One downloadable solc build."""

    version: str
    url: str
    sha256: str


@dataclass(frozen=True)
class _Source:
    """A release list, and the versions this platform should take from it.

    Sources are merged in order and a later one wins, which is how macOS arm64 takes the
    middle of its range from the native builds and the ends from the official list.
    """

    list_url: str
    base: str
    official: bool
    low: Version | None = None
    high: Version | None = None

    def covers(self, version: Version) -> bool:
        return not (
            (self.low is not None and version < self.low)
            or (self.high is not None and version > self.high)
        )


def use_binary(path: str | None) -> None:
    """Use `path` as the compiler, whatever version is asked for."""
    global _override
    _override = path
    ensure.cache_clear()
    _version_of_cached.cache_clear()


def override() -> str | None:
    """The compiler the user pointed sevm at, if any."""
    return _override or os.environ.get("SEVM_SOLC")


def override_version() -> str | None:
    """What the overriding compiler reports, or None when there is none.

    Version resolution asks first: an override *is* the compiler that will run, and the
    version it reports labels the build cache, so a pragma-chosen number would be a lie.
    """
    path = override()
    return _version_of_cached(path) if path else None


def platform_key() -> str:
    """This machine, named the way solc's own download directories are.

    `unsupported` means no published build exists (32-bit x86, armv7, FreeBSD, ...), not
    that sevm is broken there: `SEVM_SOLC` still points it at a compiler.
    """
    arm = platform.machine().lower() in ("aarch64", "arm64", "armv8b", "armv8l")
    if sys.platform.startswith("linux"):
        return "linux-arm64" if arm else "linux-amd64"
    if sys.platform == "darwin":
        return "macosx-arm64" if arm else "macosx-amd64"
    if sys.platform == "win32":
        # No arm64 Windows build exists; arm64 Windows runs the x86-64 one emulated.
        return "windows-amd64"
    return "unsupported"


def _sources(key: str) -> list[_Source]:
    if key == "linux-arm64":
        return [
            _Source(
                f"{_LINUX_ARM64_BUILDS}/list.json",
                _LINUX_ARM64_BUILDS,
                official=False,
                high=Version("0.8.30"),
            ),
            _Source(
                f"{_OFFICIAL}/linux-arm64/list.json",
                f"{_OFFICIAL}/linux-arm64",
                official=True,
                low=_LINUX_ARM64_OFFICIAL,
            ),
        ]
    if key == "macosx-arm64":
        return [
            _Source(
                f"{_OFFICIAL}/macosx-amd64/list.json",
                f"{_OFFICIAL}/macosx-amd64",
                official=True,
            ),
            _Source(
                f"{_MACOS_ARM64_BUILDS}/list.json",
                _MACOS_ARM64_BUILDS,
                official=False,
                low=_MACOS_ARM64_NATIVE,
                high=_MACOS_UNIVERSAL,
            ),
        ]
    if key in ("linux-amd64", "macosx-amd64", "windows-amd64"):
        return [_Source(f"{_OFFICIAL}/{key}/list.json", f"{_OFFICIAL}/{key}", True)]
    return []


def _wasm_source() -> _Source:
    """solc's Emscripten builds, published for every release and every machine."""
    base = f"{_OFFICIAL}/emscripten-wasm32"
    return _Source(f"{base}/list.json", base, official=True)


def _fetch_json(url: str) -> dict:
    response = requests.get(url, headers=_HEADERS, timeout=_TIMEOUT)
    response.raise_for_status()
    return response.json()


def _download(url: str) -> bytes:
    response = requests.get(url, headers=_HEADERS, timeout=_TIMEOUT)
    response.raise_for_status()
    return response.content


def _releases(source: _Source) -> dict[str, Release]:
    """One release list, parsed into `version -> Release`.

    The official list names each file in `builds[].path`; the third-party lists carry only
    `{version, sha256}` and keep the filenames in a separate `releases` map.
    """
    payload = _fetch_json(source.list_url)
    names = payload.get("releases") or {}
    found: dict[str, Release] = {}
    for build in payload.get("builds") or []:
        version = str(build.get("version", ""))
        path = build.get("path") if source.official else names.get(version)
        digest = str(build.get("sha256", "")).removeprefix("0x")
        if not version or not path or not digest:
            continue
        try:
            if not source.covers(Version(version)):
                continue
        except InvalidVersion:
            continue
        found[version] = Release(version, f"{source.base}/{path}", digest)
    return found


def _fetch_index(key: str) -> dict[str, list[str]]:
    index: dict[str, list[str]] = {}
    for source in _sources(key):
        for version, release in _releases(source).items():
            index[version] = [release.url, release.sha256]
    if not index:
        raise CompileError(f"no solc release list for {key}")
    return index


def release_index(key: str | None = None) -> dict[str, Release]:
    """Every solc build this platform can run, cached for a day.

    Empty when the lists cannot be fetched, so being offline degrades to whatever is
    already installed instead of failing version resolution.
    """
    key = key or platform_key()
    raw = cache.cached_json(f"solc-index-{key}.json", lambda: _fetch_index(key))
    if not isinstance(raw, dict):
        return {}
    return {
        version: Release(version, entry[0], entry[1])
        for version, entry in raw.items()
        if isinstance(entry, list) and len(entry) == 2
    }


def available_versions() -> list[str]:
    """Version strings that can be downloaded for this platform."""
    return list(release_index())


def install_dir() -> str:
    """Where downloads land: solcx's own folder, so both tools see one set of binaries."""
    return str(solcx.get_solcx_install_folder())


def binary_path(version: str, root: str | None = None) -> str:
    """solcx's layout — a bare file, or a directory holding solc.exe on Windows."""
    path = os.path.join(root or install_dir(), f"solc-v{version}")
    return os.path.join(path, "solc.exe") if sys.platform == "win32" else path


def _svm_path(version: str) -> str | None:
    """Foundry's svm keeps `<data dir>/<version>/solc-<version>`, already arch-correct."""
    home = os.path.expanduser("~/.svm")
    if not os.path.isdir(home):
        data = os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")
        home = os.path.join(data, "svm")
    for name in (f"solc-{version}", f"solc-{version}.exe"):
        path = os.path.join(home, version, name)
        if os.path.isfile(path):
            return path
    return None


_VERSION_RE = re.compile(r"Version:\s*(\d+\.\d+\.\d+)")


@functools.cache
def _version_of_cached(path: str) -> str | None:
    return version_of(path)


def version_of(path: str) -> str | None:
    """The version a binary reports, or None if it will not run at all.

    Running it is the only proof that it *can* run: an x86-64 download on arm64, or a
    glibc build on musl, is a perfectly good file that cannot execute.
    """
    try:
        done = subprocess.run(
            [path, "--version"], capture_output=True, timeout=30, check=False
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if done.returncode != 0:
        return None
    match = _VERSION_RE.search(done.stdout.decode("utf-8", "replace"))
    return match.group(1) if match else None


def find(version: str) -> str | None:
    """An already-usable solc of `version`, or None. Never downloads.

    `--solc-binary` and `SEVM_SOLC` override everything, including the version, because
    pointing sevm at a compiler is the escape hatch for a machine no download works on.
    """
    chosen = override()
    if chosen:
        return chosen
    for path in (binary_path(version), _svm_path(version), shutil.which("solc")):
        if path and os.path.isfile(path) and version_of(path) == version:
            return path
    return None


def installed_versions() -> list[str]:
    """Versions already on this machine, newest first."""
    found = set()
    root = install_dir()
    if os.path.isdir(root):
        for name in os.listdir(root):
            if name.startswith("solc-v"):
                found.add(name[len("solc-v") :])
    for name in list(found):
        try:
            Version(name)
        except InvalidVersion:
            found.discard(name)
    return sorted(found, key=Version, reverse=True)


def _write(payload: bytes, version: str) -> str:
    """Put a downloaded build where `binary_path` expects it, executable."""
    target = binary_path(version)
    folder = os.path.dirname(target)
    os.makedirs(folder, exist_ok=True)
    temporary = f"{target}.{os.getpid()}.tmp"
    with open(temporary, "wb") as fh:
        fh.write(payload)
    os.chmod(temporary, 0o755)
    os.replace(temporary, target)
    return target


def _write_zip(payload: bytes, version: str) -> str:
    """Old Windows releases ship a zip of solc.exe plus its dlls."""
    target = binary_path(version)
    folder = os.path.dirname(target)
    os.makedirs(folder, exist_ok=True)
    with zipfile.ZipFile(BytesIO(payload)) as archive:
        archive.extractall(folder)
    return target


def install(version: str) -> str:
    """Download `version` for this platform and return the path to it."""
    key = platform_key()
    release = release_index(key).get(version)
    if release is None:
        raise CompileError(_no_build_message(version, key))
    payload = _download(release.url)
    digest = hashlib.sha256(payload).hexdigest()
    if digest != release.sha256:
        raise CompileError(
            f"solc {version} failed its checksum ({release.url}): expected "
            f"{release.sha256}, got {digest}"
        )
    if release.url.endswith(".zip"):
        return _write_zip(payload, version)
    return _write(payload, version)


@functools.cache
def ensure(version: str) -> str:
    """The path to a working solc `version`, downloading it if this machine lacks one.

    Cached per process: every compile asks, and the answer involves running the binary.
    """
    chosen = override()
    if chosen:
        if _version_of_cached(chosen) is None:
            raise CompileError(f"the solc at {chosen} will not run on this machine")
        return chosen
    found = find(version)
    if found:
        return found
    path = install(version)
    if version_of(path) is None:
        raise CompileError(_will_not_run_message(version, path))
    return path


def wasm_index() -> dict[str, Release]:
    """Emscripten builds, cached like the native lists."""
    raw = cache.cached_json("solc-index-wasm.json", _fetch_wasm_index)
    if not isinstance(raw, dict):
        return {}
    return {
        version: Release(version, entry[0], entry[1])
        for version, entry in raw.items()
        if isinstance(entry, list) and len(entry) == 2
    }


def _fetch_wasm_index() -> dict[str, list[str]]:
    return {
        version: [release.url, release.sha256]
        for version, release in _releases(_wasm_source()).items()
    }


def wasm_dir() -> str:
    """soljson.js bundles live in sevm's cache, not `~/.solcx`, which holds binaries."""
    return os.path.join(cache.user_cache_dir(), "solc-wasm")


def ensure_wasm(version: str) -> str:
    """Path to `version`'s soljson.js, downloading it if the cache lacks it."""
    target = os.path.join(wasm_dir(), f"soljson-v{version}.js")
    if os.path.isfile(target):
        return target
    release = wasm_index().get(version)
    if release is None:
        raise CompileError(f"no WebAssembly build of solc {version} is published")
    payload = _download(release.url)
    digest = hashlib.sha256(payload).hexdigest()
    if digest != release.sha256:
        raise CompileError(
            f"solc {version} (wasm) failed its checksum ({release.url}): expected "
            f"{release.sha256}, got {digest}"
        )
    os.makedirs(wasm_dir(), exist_ok=True)
    temporary = f"{target}.{os.getpid()}.tmp"
    with open(temporary, "wb") as fh:
        fh.write(payload)
    os.replace(temporary, target)
    return target


@functools.cache
def compiler(version: str) -> Compiler:
    """The solc `version` this machine will actually compile with.

    A native binary whenever one exists and runs; otherwise solc's WebAssembly build,
    which is the only answer on musl, NixOS and architectures Solidity does not publish
    for. Cached per process: every compile asks, and answering runs a binary.
    """
    try:
        return Compiler(version, ensure(version))
    except CompileError as native:
        if wasm.runtime() is None:
            raise CompileError(
                f"{native}\n\nsolc also has a WebAssembly build that runs anywhere, but "
                "it needs a JS runtime: install node (or name one with SEVM_NODE)."
            ) from native
        try:
            return Compiler(version, ensure_wasm(version), wasm=True)
        except CompileError as fallback:
            raise CompileError(f"{native}\n\n{fallback}") from native


def _no_build_message(version: str, key: str) -> str:
    if key == "unsupported":
        return (
            f"no solc {version} build is published for {platform.system()} "
            f"{platform.machine()}. Point sevm at a compiler you built or installed "
            "yourself with SEVM_SOLC=/path/to/solc."
        )
    known = release_index(key)
    detail = "the release list could not be fetched" if not known else "no such release"
    extra = ""
    if key == "linux-arm64" and known:
        extra = " (arm64 Linux builds start at 0.5.0)"
    return (
        f"no solc {version} for {key}: {detail}{extra}. Pick a version that exists with "
        "--solc, or set SEVM_SOLC=/path/to/solc."
    )


def _will_not_run_message(version: str, path: str) -> str:
    return (
        f"solc {version} downloaded to {path} but will not run on this machine. The "
        "official Linux builds are dynamically linked against glibc, so musl systems "
        "(Alpine) need gcompat and NixOS needs a wrapped solc. Install one yourself and "
        "set SEVM_SOLC=/path/to/solc."
    )
