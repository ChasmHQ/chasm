"""The build cache: a re-run must skip solc, and a partial build must be a full build.

Everything here is hermetic (the fixture project pins solc and ships its own forge-std)
and every cache lands in tmp: `conftest.isolated_cache` redirects the user-level one, and
the project-level one lives inside the copied fixture.
"""

from __future__ import annotations

import gzip
import json
import os
import shutil

import pytest
from conftest import HERE

from sevm import cache
from sevm import compile as C
from sevm.compile import compile_foundry_project


@pytest.fixture
def root(tmp_path) -> str:
    """tests/foundry_project copied out of the repo, so its cache lands in tmp."""
    dest = str(tmp_path / "foundry_project")
    shutil.copytree(os.path.join(HERE, "foundry_project"), dest)
    return dest


def build(root: str, **kwargs):
    kwargs.setdefault("install_missing", False)
    return compile_foundry_project(root, **kwargs)


def spy(monkeypatch) -> list:
    """Record every solc call, returning the `output_selection` each one asked for."""
    calls: list = []
    real = C.compile_standard

    def watched(*args, **kwargs):
        calls.append(kwargs.get("output_selection"))
        return real(*args, **kwargs)

    monkeypatch.setattr(C, "compile_standard", watched)
    return calls


def shape(project) -> dict:
    """Everything the debugger maps against, for comparing two builds."""
    return {
        "ids": {k: s.file_id for k, s in project.sources.items()},
        "asts": project.asts,
        "artifacts": {
            name: (
                art.abi,
                art.bytecode,
                art.deployed_bytecode,
                art.source_map,
                art.deployed_source_map,
                art.storage_layout,
                art.method_identifiers,
                art.immutable_references,
                art.source_range,
            )
            for name, art in project.artifacts.items()
        },
    }


def edit_token(root: str) -> None:
    """Change Token.sol's code, which also changes the test contract that deploys it."""
    path = os.path.join(root, "src", "Token.sol")
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(text.replace("not owner", "caller is not the owner"))


# ==================================================================
# hits
# ==================================================================


def test_second_build_never_reaches_solc(root, monkeypatch):
    first = build(root)
    monkeypatch.setattr(
        C, "compile_standard", lambda *a, **k: pytest.fail("solc was invoked")
    )
    notices: list[str] = []
    assert shape(build(root, on_notice=notices.append)) == shape(first)
    assert any("cache hit" in n for n in notices)


def test_cache_lives_in_the_foundry_cache_dir(root):
    build(root)
    entries = os.listdir(os.path.join(root, "cache", "sevm"))
    assert "index.json" in entries
    assert any(name.endswith(".json.gz") for name in entries)


def test_a_plain_directory_is_left_alone(tmp_path):
    plain = tmp_path / "plain"
    plain.mkdir()
    (plain / "A.sol").write_text("pragma solidity ^0.8.20;\ncontract A {}\n")
    build(str(plain), solc_version=C.DEFAULT_SOLC_VERSION)
    assert os.listdir(plain) == ["A.sol"]
    assert os.path.isdir(cache.cache_dir(str(plain)))


def test_no_cache_neither_reads_nor_writes(root, monkeypatch):
    calls = spy(monkeypatch)
    build(root, use_cache=False)
    build(root, use_cache=False)
    assert len(calls) == 2
    assert not os.path.exists(os.path.join(root, "cache"))
    assert not os.path.exists(os.path.join(root, "out"))


def test_env_switch_disables_the_cache(root, monkeypatch):
    monkeypatch.setenv("SEVM_NO_CACHE", "1")
    build(root)
    assert not os.path.exists(os.path.join(root, "cache"))
    assert not os.path.exists(os.path.join(root, "out"))


def test_force_recompiles_and_rewrites(root, monkeypatch):
    build(root)
    calls = spy(monkeypatch)
    forced = build(root, force=True)
    assert len(calls) == 1
    assert calls[0] is None  # a full request, not a narrowed one
    assert forced.artifact("Token") is not None


@pytest.mark.parametrize("changed", [{"optimize": True}, {"evm_version": "paris"}])
def test_different_settings_are_a_different_unit(root, monkeypatch, changed):
    build(root)
    calls = spy(monkeypatch)
    build(root, **changed)
    assert len(calls) == 1


# ==================================================================
# out/sevm artifacts
# ==================================================================


def read_artifact(root: str, source: str, name: str) -> dict:
    path = os.path.join(root, "out", "sevm", source, f"{name}.json")
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def test_artifacts_are_written_in_forge_shape(root):
    project = build(root)
    token = read_artifact(root, "Token.sol", "Token")
    art = project.artifact("Token")

    assert token["sourceName"] == "src/Token.sol"
    assert token["bytecode"]["object"] == "0x" + art.bytecode.hex()
    assert token["deployedBytecode"]["object"] == "0x" + art.deployed_bytecode.hex()
    assert token["deployedBytecode"]["sourceMap"] == art.deployed_source_map
    assert token["methodIdentifiers"] == art.method_identifiers
    assert token["storageLayout"] == art.storage_layout
    assert token["id"] == project.sources["src/Token.sol"].file_id
    assert [e["name"] for e in token["abi"] if e["type"] == "function"] == [
        "balanceOf",
        "mint",
        "owner",
    ]
    # The nesting is what keeps forge's own artifacts out of reach.
    assert not os.path.exists(os.path.join(root, "out", "Token.sol"))


def test_two_sources_with_one_basename_both_land(root):
    vendor = os.path.join(root, "vendor")
    os.makedirs(vendor)
    with open(os.path.join(vendor, "Token.sol"), "w", encoding="utf-8") as fh:
        fh.write("pragma solidity ^0.8.20;\ncontract Token { uint256 public x; }\n")

    build(root)
    assert read_artifact(root, "Token.sol", "Token")["sourceName"] == "src/Token.sol"
    assert read_artifact(root, "Token.sol", "Token.1")["sourceName"] == (
        "vendor/Token.sol"
    )


def test_a_forge_artifact_is_never_overwritten(root):
    forge_own = os.path.join(root, "out", "Token.sol", "Token.json")
    os.makedirs(os.path.dirname(forge_own))
    with open(forge_own, "w", encoding="utf-8") as fh:
        json.dump({"builtBy": "forge"}, fh)

    build(root)
    with open(forge_own, encoding="utf-8") as fh:
        assert json.load(fh) == {"builtBy": "forge"}


def test_artifacts_follow_the_out_key_in_foundry_toml(root):
    toml = os.path.join(root, "foundry.toml")
    with open(toml, encoding="utf-8") as fh:
        text = fh.read()
    with open(toml, "w", encoding="utf-8") as fh:
        fh.write(text.replace('src = "src"', 'src = "src"\nout = "artifacts"'))

    build(root)
    assert os.path.isfile(
        os.path.join(root, "artifacts", "sevm", "Token.sol", "Token.json")
    )


def test_artifacts_are_rewritten_when_the_out_tree_is_deleted(root):
    build(root)
    shutil.rmtree(os.path.join(root, "out"))
    build(root)  # a cache hit still restores what was removed
    assert os.path.isfile(os.path.join(root, "out", "sevm", "Token.sol", "Token.json"))


def test_an_edit_reaches_the_written_artifact(root):
    build(root)
    before = read_artifact(root, "Token.sol", "Token")["deployedBytecode"]["object"]
    edit_token(root)
    project = build(root)
    after = read_artifact(root, "Token.sol", "Token")["deployedBytecode"]["object"]
    assert after != before
    assert after == "0x" + project.artifact("Token").deployed_bytecode.hex()


# ==================================================================
# partial builds
# ==================================================================


def test_partial_build_equals_a_full_one(root, monkeypatch):
    build(root)
    edit_token(root)

    calls = spy(monkeypatch)
    notices: list[str] = []
    partial = build(root, on_notice=notices.append)
    assert len(calls) == 1
    # Token.sol changed and Token.t.sol deploys it, so both are rebuilt and forge-std is not.
    assert sorted(calls[0]) == ["src/Token.sol", "test/Token.t.sol"]
    assert any("recompiled 2 of 5 sources" in n for n in notices)

    shutil.rmtree(os.path.join(root, "cache"))
    assert shape(partial) == shape(build(root, use_cache=False))


def test_an_edit_reaches_the_contract_that_embeds_it(root):
    before = build(root).artifact("TokenTest").bytecode
    edit_token(root)
    after = build(root).artifact("TokenTest").bytecode
    # TokenTest's constructor carries Token's creation code, so reusing it would be stale.
    assert after != before


def test_a_new_source_falls_back_to_a_full_build(root, monkeypatch):
    build(root)
    with open(os.path.join(root, "src", "Extra.sol"), "w", encoding="utf-8") as fh:
        fh.write("pragma solidity ^0.8.20;\ncontract Extra {}\n")

    calls = spy(monkeypatch)
    project = build(root)
    # Source ids shift when the set changes, so nothing may be reused from the old unit.
    assert calls == [None]
    assert project.artifact("Extra") is not None
    assert shape(project) == shape(build(root, use_cache=False))


def test_dependents_walks_importers_transitively():
    edges = {"a.sol": ["b.sol"], "b.sol": ["c.sol"], "d.sol": []}
    assert cache.dependents(edges, {"c.sol"}) == {"a.sol", "b.sol", "c.sol"}
    assert cache.dependents(edges, {"a.sol"}) == {"a.sol"}


def test_merge_refuses_a_shifted_source_id():
    base = {"sources": {"a.sol": {"id": 0, "ast": {}}}, "contracts": {}}
    fresh = {"sources": {"a.sol": {"id": 1}}, "contracts": {}}
    assert cache.merge_output(base, fresh, dirty=set()) is None
    assert cache.merge_output(base, {"sources": {"a.sol": {"id": 0}}}, set()) is not None


# ==================================================================
# damaged cache
# ==================================================================


def test_a_truncated_unit_only_costs_a_recompile(root, monkeypatch):
    build(root)
    directory = os.path.join(root, "cache", "sevm")
    unit = next(n for n in os.listdir(directory) if n.endswith(".json.gz"))
    with open(os.path.join(directory, unit), "wb") as fh:
        fh.write(gzip.compress(b'{"sources": {"trunc'))

    calls = spy(monkeypatch)
    assert build(root).artifact("Token") is not None
    assert len(calls) == 1
    assert cache.BuildCache(directory).load(unit[: -len(".json.gz")]) is not None


def test_a_corrupt_index_only_costs_a_recompile(root, monkeypatch):
    build(root)
    index = os.path.join(root, "cache", "sevm", "index.json")
    with open(index, "w", encoding="utf-8") as fh:
        fh.write("{not json")
    edit_token(root)

    # No index means no base to build partially against; the full compile rewrites it.
    calls = spy(monkeypatch)
    assert build(root).artifact("Token") is not None
    assert calls == [None]
    with open(index, encoding="utf-8") as fh:
        assert json.loads(fh.read())["units"]


def test_an_unwritable_cache_dir_is_not_fatal(root, monkeypatch):
    monkeypatch.setattr(
        cache, "_write_atomic", lambda *a: (_ for _ in ()).throw(OSError("read-only"))
    )
    assert build(root).artifact("Token") is not None


# ==================================================================
# storage
# ==================================================================


def test_prune_keeps_the_newest_units(tmp_path):
    store = cache.BuildCache(str(tmp_path / "units"))
    doc = {"sources": {}, "contracts": {}}
    for n in range(8):
        store.store(f"unit{n}", doc, "settings", {"a.sol": str(n)})
    kept = [f for f in os.listdir(store.dir) if f.endswith(".json.gz")]
    assert len(kept) == 5
    assert store.load("unit7") is not None
    assert store.load("unit0") is None


def test_base_is_the_newest_unit_with_the_same_settings_and_sources(tmp_path):
    store = cache.BuildCache(str(tmp_path / "units"))
    store.store("old", {"sources": {"a": {}}, "contracts": {}}, "s1", {"a.sol": "1"})
    store.store("new", {"sources": {"b": {}}, "contracts": {}}, "s1", {"a.sol": "2"})
    store.store("other", {"sources": {"c": {}}, "contracts": {}}, "s2", {"a.sol": "3"})

    doc, hashes = store.base_for("s1", {"a.sol"})
    assert list(doc["sources"]) == ["b"] and hashes == {"a.sol": "2"}
    assert store.base_for("s3", {"a.sol"}) is None
    assert store.base_for("s1", {"a.sol", "b.sol"}) is None


# ==================================================================
# solc release list
# ==================================================================


def test_release_list_is_fetched_once_a_day():
    calls = []

    def fetch():
        calls.append(1)
        return ["0.8.28", "0.8.29"]

    assert cache.installable_versions(fetch) == ["0.8.28", "0.8.29"]
    assert cache.installable_versions(fetch) == ["0.8.28", "0.8.29"]
    assert len(calls) == 1
    cache.installable_versions(fetch, ttl=-1)
    assert len(calls) == 2


def test_release_list_offline_is_empty_not_an_error():
    def boom():
        raise ConnectionError("offline")

    assert cache.installable_versions(boom) == []
