"""What sevm found on this machine, and what it would do about it.

Every question this answers came from a real support round: which build of solc this
platform gets, whether the one already installed runs, whether the WebAssembly fallback
is reachable, and which environment variable is quietly overriding the lot. It resolves
nothing it would not resolve anyway, and downloads nothing — a missing compiler is
reported as the URL it would be fetched from, so the answer is the same whether or not
the machine has ever run a compile.
"""

from __future__ import annotations

import os
import platform
import shutil
import subprocess
import sys
from dataclasses import dataclass

from . import cache
from .compile import DEFAULT_SOLC_VERSION, solcbin, wasm


@dataclass(frozen=True)
class Line:
    """One reported fact. `problem` marks the ones that stop sevm working."""

    label: str
    value: str
    problem: bool = False


def _libc() -> str:
    """glibc's version, or a note that this is not glibc.

    Worth reporting because Solidity's Linux builds are dynamically linked against it,
    so a musl machine gets a binary that downloads fine and cannot execute.
    """
    if not sys.platform.startswith("linux"):
        return ""
    name, version = platform.libc_ver()
    if name and version:
        return f", {name} {version}"
    return ", musl or unknown libc"


def _tool_version(command: str, *args: str) -> str | None:
    path = shutil.which(command)
    if not path:
        return None
    try:
        done = subprocess.run([path, *args], capture_output=True, timeout=30, check=False)
    except (OSError, subprocess.SubprocessError):
        return None
    if done.returncode != 0:
        return None
    return done.stdout.decode("utf-8", "replace").strip().splitlines()[0]


def _solc_lines(version: str, node: str | None) -> list[Line]:
    lines: list[Line] = []
    override = solcbin.override()
    if override:
        reported = solcbin.override_version()
        lines.append(
            Line(
                "solc",
                f"{override} (SEVM_SOLC or --solc-binary)"
                + (f", reports {reported}" if reported else ", DOES NOT RUN"),
                problem=reported is None,
            )
        )
        return lines

    found = solcbin.find(version)
    index = solcbin.release_index()
    if found:
        lines.append(Line("solc", f"{version} at {found}"))
    elif version in index:
        lines.append(
            Line("solc", f"{version} would be fetched from {index[version].url}")
        )

    installed = solcbin.installed_versions()
    lines.append(Line("installed", ", ".join(installed) if installed else "none"))
    if index:
        versions = sorted(index, key=lambda v: tuple(int(p) for p in v.split(".")))
        lines.append(
            Line("available", f"{len(index)} releases, {versions[0]} .. {versions[-1]}")
        )
        return lines

    # No list at all: either this platform has no published build, or the fetch failed.
    # Only a problem if nothing else here can compile, which the wasm build usually can.
    detail = f"no release list for {solcbin.platform_key()}"
    if node:
        detail += " — solc's WebAssembly build will be used instead"
    else:
        detail += " (offline, or nothing is published for this platform)"
    lines.append(Line("available", detail, problem=not (found or installed or node)))
    return lines


def report() -> list[Line]:
    """Everything `sevm doctor` prints, in order."""
    node = wasm.runtime()
    node_version = _tool_version(node, "--version") if node else None
    git = _tool_version("git", "--version")

    lines = [
        Line("sevm", _sevm_version()),
        Line(
            "python",
            f"{platform.python_version()} ({platform.python_implementation()}) "
            f"at {sys.executable}",
        ),
        Line(
            "platform",
            f"{solcbin.platform_key()} ({platform.system()} {platform.machine()}"
            f"{_libc()})",
        ),
        Line("git", git or "not found — library resolution needs it", problem=not git),
    ]
    lines += _solc_lines(DEFAULT_SOLC_VERSION, node)
    lines.append(
        Line(
            "wasm solc",
            f"{node} {node_version}" if node else "no JS runtime (install node)",
        )
    )
    lines.append(Line("binaries", solcbin.install_dir()))
    lines.append(Line("cache", cache.user_cache_dir()))

    overrides = [
        f"{name}={os.environ[name]}"
        for name in ("SEVM_SOLC", "SEVM_NODE", "SOLCX_BINARY_PATH", "SEVM_NO_CACHE")
        if os.environ.get(name)
    ]
    if overrides:
        lines.append(Line("overrides", " ".join(overrides)))
    return lines


def _sevm_version() -> str:
    from importlib.metadata import PackageNotFoundError, version

    try:
        return version("sevm")
    except PackageNotFoundError:  # pragma: no cover - running from a source tree
        return "unknown"


def ok(lines: list[Line]) -> bool:
    """Whether everything reported is in working order. The exit status.

    A machine that can compile but has no git still fails: library resolution needs it,
    and finding that out from `doctor` beats finding it out from a failed import.
    """
    return not any(line.problem for line in lines)


def render(lines: list[Line]) -> str:
    width = max(len(line.label) for line in lines)
    return "\n".join(
        f"{'!' if line.problem else ' '} {line.label:<{width}}  {line.value}"
        for line in lines
    )
