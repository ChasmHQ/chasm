"""Finding a solc binary for this machine.

Every test here is offline: the release lists and the downloads are faked, because what
has to be right is the platform map (which list a version comes from), the checksum gate,
and the order local binaries are preferred in. Nothing may write to the developer's real
`~/.solcx` either, so `install_root` redirects it per test.
"""

from __future__ import annotations

import copy
import hashlib
import os

import pytest

from sevm.compile import CompileError, solcbin

# Shaped like binaries.soliditylang.org: filenames live on the build entries, and the
# digest is 0x-prefixed. The versions are the ones the platform boundaries turn on.
OFFICIAL_LIST = {
    "builds": [
        {"path": "solc-v0.8.20", "version": "0.8.20", "sha256": "0xaaaa"},
        {"path": "solc-v0.8.28", "version": "0.8.28", "sha256": "0xbbbb"},
        {"path": "solc-v0.8.31", "version": "0.8.31", "sha256": "0xcccc"},
    ]
}
# Shaped like nikitastupin/solc and alloy-rs/solc-builds: no `path`, filenames live in a
# separate `releases` map, digests bare.
THIRD_PARTY_LIST = {
    "builds": [
        {"version": "0.8.20", "sha256": "dddd"},
        {"version": "0.8.28", "sha256": "eeee"},
        {"version": "0.8.31", "sha256": "ffff"},
    ],
    "releases": {
        "0.8.20": "solc-v0.8.20",
        "0.8.28": "solc-v0.8.28",
        "0.8.31": "solc-v0.8.31",
    },
}


@pytest.fixture
def install_root(tmp_path, monkeypatch):
    """Send downloads to tmp instead of ~/.solcx, and forget the per-process cache."""
    root = tmp_path / "solcx"
    root.mkdir()
    monkeypatch.setenv("SOLCX_BINARY_PATH", str(root))
    monkeypatch.delenv("SEVM_SOLC", raising=False)
    monkeypatch.setattr(solcbin, "install_dir", lambda: str(root))
    solcbin.ensure.cache_clear()
    yield str(root)
    solcbin.use_binary(None)
    solcbin.ensure.cache_clear()


@pytest.fixture
def lists(monkeypatch):
    """Serve a fake release list per URL, and record which ones were asked for."""
    asked = []

    def fetch(url):
        asked.append(url)
        official = "binaries.soliditylang.org" in url
        return copy.deepcopy(OFFICIAL_LIST if official else THIRD_PARTY_LIST)

    monkeypatch.setattr(solcbin, "_fetch_json", fetch)
    return asked


# -- the platform map --------------------------------------------------------


@pytest.mark.parametrize(
    ("platform_name", "machine", "expected"),
    [
        ("linux", "x86_64", "linux-amd64"),
        ("linux", "aarch64", "linux-arm64"),
        ("linux", "armv7l", "linux-amd64"),  # no arm32 build; amd64 is the only list
        ("darwin", "x86_64", "macosx-amd64"),
        ("darwin", "arm64", "macosx-arm64"),
        ("win32", "AMD64", "windows-amd64"),
        ("win32", "ARM64", "windows-amd64"),  # emulated, as svm does it
        ("freebsd13", "amd64", "unsupported"),
    ],
)
def test_platform_key(monkeypatch, platform_name, machine, expected):
    monkeypatch.setattr(solcbin.sys, "platform", platform_name)
    monkeypatch.setattr(solcbin.platform, "machine", lambda: machine)
    assert solcbin.platform_key() == expected


def test_linux_arm64_switches_lists_at_the_official_cutover(lists):
    index = solcbin.release_index("linux-arm64")

    # Below 0.8.31 only the third-party repository has arm64 builds, even for versions
    # the official list also carries (it carries them for amd64).
    assert "nikitastupin" in index["0.8.20"].url
    assert "nikitastupin" in index["0.8.28"].url
    assert index["0.8.28"].sha256 == "eeee"
    # From 0.8.31 Solidity publishes its own, and those win where the lists overlap.
    assert index["0.8.31"].url.startswith("https://binaries.soliditylang.org/linux-arm64")
    assert index["0.8.31"].sha256 == "cccc"


def test_macos_arm64_takes_native_builds_only_where_there_is_no_universal_one(lists):
    index = solcbin.release_index("macosx-arm64")

    # 0.8.5-0.8.24 has no arm64 build from Solidity and the x86 one needs Rosetta.
    assert "alloy-rs" in index["0.8.20"].url
    # Past 0.8.24 the official macOS binaries are universal, so they are used.
    assert "binaries.soliditylang.org/macosx-amd64" in index["0.8.28"].url
    assert index["0.8.28"].sha256 == "bbbb"


def test_amd64_platforms_use_one_official_list(lists):
    solcbin.release_index("linux-amd64")
    assert lists == ["https://binaries.soliditylang.org/linux-amd64/list.json"]


def test_an_unsupported_platform_has_no_releases(lists):
    assert solcbin.release_index("unsupported") == {}


def test_a_release_list_that_cannot_be_fetched_is_empty_not_an_error(monkeypatch):
    def boom(url):
        raise ConnectionError("offline")

    monkeypatch.setattr(solcbin, "_fetch_json", boom)
    assert solcbin.release_index("linux-arm64") == {}
    assert solcbin.available_versions() == []


# -- installing --------------------------------------------------------------


def _serve(monkeypatch, payload: bytes, digest: str | None = None) -> None:
    """A one-version arm64 list whose digest is `digest`, downloading to `payload`."""
    listing = {
        "builds": [
            {"version": "0.8.28", "sha256": digest or hashlib.sha256(payload).hexdigest()}
        ],
        "releases": {"0.8.28": "solc-v0.8.28"},
    }
    monkeypatch.setattr(solcbin, "platform_key", lambda: "linux-arm64")
    monkeypatch.setattr(solcbin, "_fetch_json", lambda url: copy.deepcopy(listing))
    monkeypatch.setattr(solcbin, "_download", lambda url: payload)


def test_install_refuses_a_download_that_fails_its_checksum(install_root, monkeypatch):
    _serve(monkeypatch, b"#!/bin/sh\necho solc\n", digest="cccc")
    with pytest.raises(CompileError, match="checksum"):
        solcbin.install("0.8.28")
    assert not os.path.exists(solcbin.binary_path("0.8.28", install_root))


def test_install_writes_an_executable_when_the_checksum_matches(
    install_root, monkeypatch
):
    _serve(monkeypatch, b"#!/bin/sh\necho solc\n")
    path = solcbin.install("0.8.28")
    assert path == solcbin.binary_path("0.8.28", install_root)
    assert os.path.exists(path) and os.access(path, os.X_OK)


def test_a_version_this_platform_cannot_download_says_where_to_look(
    install_root, lists, monkeypatch
):
    monkeypatch.setattr(solcbin, "platform_key", lambda: "linux-arm64")
    with pytest.raises(CompileError, match="SEVM_SOLC"):
        solcbin.ensure("0.4.24")


def test_a_binary_that_will_not_run_here_is_reported_as_such(install_root, monkeypatch):
    # What arrives on musl or the wrong arch: a correct file that cannot execute.
    _serve(monkeypatch, b"not an executable")
    with pytest.raises(CompileError, match="will not run"):
        solcbin.ensure("0.8.28")


# -- finding what is already here --------------------------------------------


def _fake_solc(path: str, version: str) -> str:
    """A script that answers `--version` the way solc does."""
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(
            "#!/bin/sh\n"
            "echo 'solc, the solidity compiler commandline interface'\n"
            f"echo 'Version: {version}+commit.7893614a.Linux.g++'\n"
        )
    os.chmod(path, 0o755)
    return path


@pytest.mark.skipif(os.name != "posix", reason="writes a shell script")
def test_version_of_reads_what_the_binary_reports(tmp_path):
    path = _fake_solc(str(tmp_path / "solc"), "0.8.28")
    assert solcbin.version_of(path) == "0.8.28"
    assert solcbin.version_of(str(tmp_path / "missing")) is None


@pytest.mark.skipif(os.name != "posix", reason="writes a shell script")
def test_an_installed_binary_is_used_without_downloading(install_root, monkeypatch):
    _fake_solc(solcbin.binary_path("0.8.28", install_root), "0.8.28")

    def no_downloads(url):
        raise AssertionError("downloaded a solc that was already installed")

    monkeypatch.setattr(solcbin, "_download", no_downloads)
    assert solcbin.ensure("0.8.28") == solcbin.binary_path("0.8.28", install_root)
    assert solcbin.installed_versions() == ["0.8.28"]


@pytest.mark.skipif(os.name != "posix", reason="writes a shell script")
def test_foundrys_svm_binaries_count_as_installed(install_root, tmp_path, monkeypatch):
    # svm keeps `<home>/.svm/<version>/solc-<version>`, already built for this arch.
    home = tmp_path / "home"
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setattr(os.path, "expanduser", lambda p: p.replace("~", str(home)))
    svm = _fake_solc(str(home / ".svm" / "0.8.28" / "solc-0.8.28"), "0.8.28")

    monkeypatch.setattr(
        solcbin, "_download", lambda url: pytest.fail("should reuse svm's binary")
    )
    assert solcbin.ensure("0.8.28") == svm


@pytest.mark.skipif(os.name != "posix", reason="writes a shell script")
def test_a_binary_reporting_another_version_is_not_used(install_root, monkeypatch):
    _fake_solc(solcbin.binary_path("0.8.28", install_root), "0.8.27")
    assert solcbin.find("0.8.28") is None


@pytest.mark.skipif(os.name != "posix", reason="writes a shell script")
def test_sevm_solc_overrides_everything(install_root, tmp_path, monkeypatch):
    path = _fake_solc(str(tmp_path / "own-solc"), "0.8.28")
    monkeypatch.setenv("SEVM_SOLC", path)
    # Even a version this platform has no build for at all.
    assert solcbin.find("0.4.24") == path
    assert solcbin.ensure("0.4.24") == path


@pytest.mark.skipif(os.name != "posix", reason="writes a shell script")
def test_an_override_binary_names_its_own_version(install_root, tmp_path):
    solcbin.use_binary(_fake_solc(str(tmp_path / "own-solc"), "0.8.19"))
    assert solcbin.override_version() == "0.8.19"


def test_an_override_that_will_not_run_is_refused(install_root, tmp_path):
    bad = tmp_path / "bad-solc"
    bad.write_text("not an executable")
    bad.chmod(0o755)
    solcbin.use_binary(str(bad))
    assert solcbin.override_version() is None
    with pytest.raises(CompileError, match="will not run"):
        solcbin.ensure("0.8.28")
