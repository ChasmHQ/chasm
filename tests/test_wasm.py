"""solc's WebAssembly build, the compiler of last resort.

It is what runs where no native binary can: musl, NixOS, an architecture Solidity does not
publish for. These tests drive the JS runtime with a stub script instead of node, so the
plumbing (arguments, stdin, exit codes, error severity) is exercised offline; the
end-to-end test that a real soljson.js agrees with the native compiler is marked
`network` and skipped unless SEVM_NETWORK_TESTS=1.
"""

from __future__ import annotations

import copy
import json
import os

import pytest

from sevm.compile import CompileError, solcbin, wasm
from sevm.compile.solc import _OUTPUT_SELECTION

pytestmark = pytest.mark.skipif(os.name != "posix", reason="stubs a runtime with sh")


@pytest.fixture
def choosing_afresh():
    """`compiler` memoises per process; these tests each want their own answer.

    The memoised functions are captured up front because a test may have replaced the
    module attributes by the time this unwinds — monkeypatch undoes its own work last.
    """
    cached = (solcbin.compiler, solcbin.ensure)
    for function in cached:
        function.cache_clear()
    yield
    for function in cached:
        function.cache_clear()


def _stub_runtime(tmp_path, monkeypatch, body: str) -> str:
    """A stand-in for node: `SEVM_NODE` points at it, and it prints `body` on stdout."""
    path = tmp_path / "fake-node"
    path.write_text(f"#!/bin/sh\ncat > /dev/null\n{body}\n")
    path.chmod(0o755)
    monkeypatch.setenv("SEVM_NODE", str(path))
    return str(path)


# -- the runtime -------------------------------------------------------------


def test_sevm_node_names_the_runtime(tmp_path, monkeypatch):
    path = _stub_runtime(tmp_path, monkeypatch, "true")
    assert wasm.runtime() == path


def test_no_runtime_says_what_to_install(monkeypatch):
    monkeypatch.delenv("SEVM_NODE", raising=False)
    monkeypatch.setattr(wasm.shutil, "which", lambda name: None)
    assert wasm.runtime() is None
    with pytest.raises(CompileError, match="Install node"):
        wasm.compile_standard("/nowhere/soljson.js", {})


# -- driving it --------------------------------------------------------------


def test_output_is_read_back_from_stdout(tmp_path, monkeypatch):
    _stub_runtime(tmp_path, monkeypatch, 'echo \'{"contracts": {"A.sol": {}}}\'')
    assert wasm.compile_standard("soljson.js", {}) == {"contracts": {"A.sol": {}}}


def test_a_compile_error_is_raised_not_returned(tmp_path, monkeypatch):
    # solc reports a failed compile in the document, exiting 0 either way.
    document = json.dumps(
        {
            "errors": [
                {"severity": "warning", "formattedMessage": "unused variable"},
                {"severity": "error", "formattedMessage": "A.sol:3: Expected ';'"},
            ]
        }
    )
    _stub_runtime(tmp_path, monkeypatch, f"cat <<'EOF'\n{document}\nEOF")
    with pytest.raises(CompileError, match=r"Expected ';'"):
        wasm.compile_standard("soljson.js", {})


def test_a_runtime_that_fails_reports_its_stderr(tmp_path, monkeypatch):
    _stub_runtime(tmp_path, monkeypatch, "echo 'no such module' >&2\nexit 1")
    with pytest.raises(CompileError, match="no such module"):
        wasm.compile_standard("soljson.js", {})


def test_output_that_is_not_json_is_an_error(tmp_path, monkeypatch):
    _stub_runtime(tmp_path, monkeypatch, "echo 'Segmentation fault'")
    with pytest.raises(CompileError, match="no JSON"):
        wasm.compile_standard("soljson.js", {})


# -- the one difference between the backends ---------------------------------


def test_builtin_ids_are_signed_back(tmp_path, monkeypatch):
    # The Emscripten build writes solidity's negative builtin ids as unsigned 32-bit.
    document = {
        "sources": {
            "A.sol": {
                "id": 0,
                "ast": {
                    "id": 12,
                    "nodes": [
                        {
                            "id": 34,
                            "referencedDeclaration": 4294967281,  # -15, `msg`
                            "overloadedDeclarations": [4294967278, 7],
                        }
                    ],
                },
            }
        }
    }
    _stub_runtime(tmp_path, monkeypatch, f"cat <<'EOF'\n{json.dumps(document)}\nEOF")
    node = wasm.compile_standard("soljson.js", {})["sources"]["A.sol"]["ast"]["nodes"][0]

    assert node["referencedDeclaration"] == -15
    assert node["overloadedDeclarations"] == [-18, 7]
    # Real node ids are positive and must be left alone.
    assert node["id"] == 34


# -- choosing it -------------------------------------------------------------


def test_wasm_is_used_when_no_native_build_runs(choosing_afresh, monkeypatch):
    def no_native(version):
        raise CompileError("no solc 0.8.28 for riscv64")

    monkeypatch.setattr(solcbin, "ensure", no_native)
    monkeypatch.setattr(wasm, "runtime", lambda: "/usr/bin/node")
    monkeypatch.setattr(solcbin, "ensure_wasm", lambda version: "/cache/soljson.js")

    chosen = solcbin.compiler("0.8.28")
    assert chosen.wasm and chosen.path == "/cache/soljson.js"


def test_without_a_runtime_the_native_failure_stands(choosing_afresh, monkeypatch):
    def no_native(version):
        raise CompileError("no solc 0.8.28 for riscv64")

    monkeypatch.setattr(solcbin, "ensure", no_native)
    monkeypatch.setattr(wasm, "runtime", lambda: None)

    with pytest.raises(CompileError) as caught:
        solcbin.compiler("0.8.28")
    assert "riscv64" in str(caught.value) and "SEVM_NODE" in str(caught.value)


# -- against the real thing --------------------------------------------------


@pytest.mark.network
def test_wasm_agrees_with_the_native_compiler(choosing_afresh, monkeypatch):
    """The build cache is keyed on the solc version, never on which backend ran."""
    with open(os.path.join(os.path.dirname(__file__), "contracts", "Bank.sol")) as fh:
        source = fh.read()
    payload = {
        "language": "Solidity",
        "sources": {"Bank.sol": {"content": source}},
        "settings": {
            "optimizer": {"enabled": False},
            "outputSelection": _OUTPUT_SELECTION,
        },
    }
    native = solcbin.compiler("0.8.28").compile(copy.deepcopy(payload))

    solcbin.compiler.cache_clear()
    solcbin.ensure.cache_clear()
    monkeypatch.setattr(solcbin, "find", lambda version: None)
    monkeypatch.setattr(solcbin, "platform_key", lambda: "unsupported")
    fallback = solcbin.compiler("0.8.28")

    assert fallback.wasm
    assert fallback.compile(copy.deepcopy(payload)) == native
