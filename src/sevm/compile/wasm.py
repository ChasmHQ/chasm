"""Compiling with solc's WebAssembly build, for machines no native binary runs on.

Alpine and other musl systems, NixOS, riscv64, FreeBSD: `solcbin` has no build to offer,
or the one it downloads cannot execute. Solidity publishes an Emscripten build of every
release back to 0.3.6, and it produces byte-identical bytecode to the native compiler, so
the build cache stays valid across the two.

It costs a JS runtime: soljson.js is an Emscripten bundle, not a WASI module, so wasmtime
and friends cannot load it and there is no pure-Python option. `node` is what sevm looks
for (`SEVM_NODE` names another). Loading the ~9MB bundle and compiling a small contract
takes ~0.4s end to end against ~0.01s native, which is the reason this is a fallback and
not the default.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from typing import Any

from .model import CompileError

DRIVER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "soljson.js")

_RUNTIMES = ("node",)


def runtime() -> str | None:
    """The JS runtime to drive soljson.js with, or None if the machine has none."""
    named = os.environ.get("SEVM_NODE")
    candidates = (named,) if named else _RUNTIMES
    for candidate in candidates:
        found = shutil.which(candidate)
        if found:
            return found
    return None


def compile_standard(soljson: str, payload: dict) -> dict:
    """Run one standard-JSON compile through `soljson`, raising like py-solc-x does.

    solc reports a failed compile in the output document rather than by exiting non-zero,
    so `severity == "error"` is what separates a failure from warnings — the same test
    py-solc-x applies to the native compiler, kept here so both backends fail alike.
    """
    node = runtime()
    if node is None:
        raise CompileError(
            "no JS runtime to run solc's WebAssembly build with. Install node, or name "
            "one with SEVM_NODE, or point sevm at a native compiler with SEVM_SOLC."
        )
    done = subprocess.run(
        [node, DRIVER, soljson],
        input=json.dumps(payload).encode(),
        capture_output=True,
        check=False,
    )
    if done.returncode != 0:
        detail = done.stderr.decode("utf-8", "replace").strip()
        raise CompileError(f"solc (wasm) failed: {detail}")
    try:
        output = json.loads(done.stdout)
    except ValueError as exc:
        raise CompileError(f"solc (wasm) returned no JSON: {exc}") from exc
    _raise_on_errors(output)
    return _resign_ids(output)


# Solidity gives its builtins negative ids (`msg` is -15), and the Emscripten build
# serialises them as unsigned 32-bit: `referencedDeclaration: 4294967281`. Nothing sevm
# reads resolves a builtin, but normalising keeps the two backends' documents
# interchangeable, which the build cache assumes — it is keyed on the compiler version,
# never on which backend produced the entry.
_UNSIGNED = 1 << 32
_SIGNED = 1 << 31
_ID_KEYS = frozenset(
    {
        "referencedDeclaration",
        "overloadedDeclarations",
        "overriddenDeclaration",
        "declaration",
        "baseFunctions",
    }
)


def _resign(value: Any) -> Any:
    if isinstance(value, int) and not isinstance(value, bool) and value >= _SIGNED:
        return value - _UNSIGNED
    if isinstance(value, list):
        return [_resign(item) for item in value]
    return value


def _resign_ids(node: Any) -> Any:
    if isinstance(node, dict):
        return {
            key: _resign(value) if key in _ID_KEYS else _resign_ids(value)
            for key, value in node.items()
        }
    if isinstance(node, list):
        return [_resign_ids(item) for item in node]
    return node


def _raise_on_errors(output: dict) -> None:
    errors = [
        entry for entry in output.get("errors", []) if entry.get("severity") == "error"
    ]
    if errors:
        messages = "\n".join(
            entry.get("formattedMessage") or entry.get("message", "") for entry in errors
        )
        raise CompileError(messages)
