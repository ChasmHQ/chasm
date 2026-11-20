"""Build-info import and code identity do not require network or a compiler."""

from __future__ import annotations

import copy
import json
from types import SimpleNamespace

import pytest

from chasm_debugger.compile import CompileError, compare_runtime, load_build_info
from chasm_debugger.compile.importer import project_from_build_info
from chasm_debugger.compile.model import _strip_metadata
from chasm_debugger.compile.solc import compile_standard
from chasm_debugger.evaluate import Evaluator
from chasm_debugger.evaluate.injection import EvalError
from chasm_debugger.session.code import CodeIndex, _key


def build_info():
    return {
        "solcVersion": "0.8.28",
        "input": {
            "language": "Solidity",
            "sources": {"Vault.sol": {"content": "contract Vault {}"}},
            "settings": {
                "optimizer": {"enabled": True, "runs": 999},
                "evmVersion": "cancun",
                "viaIR": False,
                "metadata": {"bytecodeHash": "none"},
            },
        },
        "output": {
            "sources": {
                "Vault.sol": {
                    "id": 0,
                    "ast": {
                        "nodeType": "SourceUnit",
                        "nodes": [
                            {
                                "nodeType": "ContractDefinition",
                                "name": "Vault",
                                "src": "0:17:0",
                            }
                        ],
                    },
                }
            },
            "contracts": {
                "Vault.sol": {
                    "Vault": {
                        "abi": [],
                        "storageLayout": {"storage": [], "types": {}},
                        "evm": {
                            "bytecode": {"object": "600000", "sourceMap": "0:17:0"},
                            "deployedBytecode": {
                                "object": "600100",
                                "sourceMap": "0:17:0",
                            },
                        },
                    }
                }
            },
        },
    }


def test_import_preserves_original_settings_and_source_ids(tmp_path):
    source = build_info()
    path = tmp_path / "build.json"
    path.write_text(json.dumps(source), encoding="utf-8")
    project = load_build_info(str(path))
    assert project.solc_version == "0.8.28"
    assert project.optimized
    assert project.compiler_settings == source["input"]["settings"]
    assert project.sources["Vault.sol"].file_id == 0
    assert project.artifact("Vault").source_range == (0, 17)
    source["input"]["settings"]["optimizer"]["runs"] = 1
    assert project.compiler_settings["optimizer"]["runs"] == 999


def test_build_info_directory_requires_unambiguous_file(tmp_path):
    for name in ("a.json", "b.json"):
        (tmp_path / name).write_text(json.dumps(build_info()), encoding="utf-8")
    with pytest.raises(CompileError, match="one build-info"):
        load_build_info(str(tmp_path))


def test_import_never_fetches_source_urls():
    source = build_info()
    source["input"]["sources"]["Vault.sol"] = {"urls": ["https://invalid/source"]}
    with pytest.raises(CompileError, match="embedded content"):
        project_from_build_info(source)


def test_import_links_only_explicit_library_addresses():
    source = build_info()
    code = source["output"]["contracts"]["Vault.sol"]["Vault"]["evm"]["deployedBytecode"]
    code["object"] = "60" + "_" * 40 + "00"
    code["linkReferences"] = {"Lib.sol": {"Lib": [{"start": 1, "length": 20}]}}
    with pytest.raises(CompileError, match="missing library"):
        project_from_build_info(source)
    source["input"]["settings"]["libraries"] = {"Lib.sol": {"Lib": "0x" + "12" * 20}}
    art = project_from_build_info(source).artifact("Vault")
    assert art.deployed_bytecode == bytes.fromhex("60" + "12" * 20 + "00")


@pytest.mark.parametrize("tail", [b"\xa2\x64ipfs", b"\x00", b"\xa1\x61x\x01"])
def test_malformed_or_unknown_metadata_is_not_stripped(tail):
    code = b"\x60\x01\x00" + tail + len(tail).to_bytes(2, "big")
    assert _strip_metadata(code) == code


def test_metadata_match_reports_raw_identity_separately():
    project = project_from_build_info(build_info())
    art = project.artifact("Vault")
    tail = bytes.fromhex("a164736f6c634300081c")
    reference = art.deployed_bytecode + tail + len(tail).to_bytes(2, "big")
    result = compare_runtime(art, reference)
    assert result["match"]
    assert not result["exact_match"]
    assert result["comparison"] == "source-map-body"
    assert result["compiled_code_hash"] != result["reference_code_hash"]


def test_equal_prefix_equal_length_code_does_not_alias():
    first = bytes([0x5B]) * 64 + bytes.fromhex("600100")
    second = bytes([0x5B]) * 64 + bytes.fromhex("600200")
    assert _key(first, False) != _key(second, False)
    assert _key(first, False) != _key(first, True)
    project = project_from_build_info(build_info())
    artifact = project.artifact("Vault")
    artifact.deployed_bytecode = first
    project.__post_init__()
    index = CodeIndex(project, {}, SimpleNamespace(), SimpleNamespace())
    assert index.artifact_for(first, False) is artifact
    assert index.artifact_for(second, False) is None


def test_standard_compiler_preserves_build_settings(monkeypatch):
    payloads = []
    monkeypatch.setattr(
        "chasm_debugger.compile.solc.solcbin.compiler",
        lambda _version: SimpleNamespace(
            compile=lambda payload: payloads.append(payload) or {}
        ),
    )
    settings = build_info()["input"]["settings"]
    original = copy.deepcopy(settings)
    compile_standard(
        {"Vault.sol": "contract Vault {}"},
        compiler_settings=settings,
        output_selection={"*": {"*": ["abi"]}},
    )
    assert payloads[0]["settings"]["optimizer"] == {"enabled": True, "runs": 999}
    assert payloads[0]["settings"]["evmVersion"] == "cancun"
    assert settings == original


def test_evaluator_preserves_imported_evm_settings(monkeypatch):
    project = project_from_build_info(build_info())
    calls = []
    monkeypatch.setattr(
        "chasm_debugger.evaluate.evaluator.compile_standard",
        lambda *_a, **kw: calls.append(kw) or {},
    )
    Evaluator(project)._try_compile(project.artifact("Vault"), "1", "uint256")
    assert calls[0]["compiler_settings"]["evmVersion"] == "cancun"
    assert calls[0]["compiler_settings"]["optimizer"]["runs"] == 999


def test_via_ir_evaluation_rejects_before_compilation():
    document = build_info()
    document["input"]["settings"]["viaIR"] = True
    project = project_from_build_info(document)
    with pytest.raises(EvalError, match="viaIR"):
        Evaluator(project)._try_compile(project.artifact("Vault"), "1", "uint256")


def test_fork_immutable_evaluation_rejects_before_compilation():
    project = project_from_build_info(build_info())
    artifact = project.artifact("Vault")
    artifact.immutable_references = {"1": [{"start": 0, "length": 1}]}
    frame = SimpleNamespace(artifact=artifact)
    with pytest.raises(EvalError, match="immutable"):
        Evaluator(project).evaluate(
            SimpleNamespace(fork_anchor={"chain_id": 1}), frame, None, "1"
        )
