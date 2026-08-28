"""Storage layout decoding, slot arithmetic, and calldata/revert decoding."""

from __future__ import annotations

from sevm.decode import (
    StorageDecoder,
    decode_calldata,
    decode_revert,
    dynamic_array_slot,
    mapping_slot,
)


def test_storage_decoder_reads_every_shape(deposit_debugger):
    dbg = deposit_debugger
    decoder = StorageDecoder(dbg.session.project.artifact("Bank").storage_layout)
    reader = lambda slot: dbg.session.inspect("read_storage", slot)  # noqa: E731
    values = {var.name: value for var, value in decoder.read_all(reader)}

    assert values["owner"].value.startswith("0x")
    assert values["feeBps"].value == 25  # packed after owner in slot 0
    assert values["totalDeposits"].value == 10**18
    assert values["name"].display == '"sevm-bank"'
    assert "mapping" in values["balances"].display
    assert values["history"].display.startswith("[0 items]")


def test_storage_decoder_packed_slot_offsets(proj):
    decoder = StorageDecoder(proj.artifact("Bank").storage_layout)
    owner = decoder.get("owner")
    fee = decoder.get("feeBps")
    assert (owner.slot, owner.offset) == (0, 0)
    assert (fee.slot, fee.offset) == (0, 20), "feeBps must share slot 0 with owner"


def test_storage_decoder_handles_missing_layout():
    decoder = StorageDecoder(None)
    assert not decoder
    assert decoder.read_all(lambda slot: 0) == []


def test_mapping_and_array_slot_arithmetic():
    from eth_utils import keccak

    key = "0x" + "11" * 20
    expected = int.from_bytes(
        keccak(bytes.fromhex("11" * 20).rjust(32, b"\x00") + (2).to_bytes(32, "big")),
        "big",
    )
    assert mapping_slot(key, 2) == expected
    assert dynamic_array_slot(4) == int.from_bytes(keccak((4).to_bytes(32, "big")), "big")


def test_decode_calldata_and_reverts(proj):
    art = proj.artifact("Bank")
    selector = next(
        bytes.fromhex(sel)
        for sig, sel in art.method_identifiers.items()
        if sig.startswith("withdraw")
    )
    data = selector + (12345).to_bytes(32, "big")
    signature, params = decode_calldata(art.abi, data)
    assert signature == "withdraw(uint256)"
    assert params[0][2] == 12345

    assert decode_calldata(art.abi, b"\x00\x00") is None
    assert decode_calldata(art.abi, b"\xde\xad\xbe\xef") is None

    from eth_abi import encode

    err = bytes.fromhex("08c379a0") + encode(["string"], ["nope"])
    assert decode_revert(err) == 'reverted: "nope"'
    panic = bytes.fromhex("4e487b71") + encode(["uint256"], [0x11])
    assert "arithmetic overflow" in decode_revert(panic)
    assert decode_revert(b"") == "reverted without a reason"


def test_decode_custom_error(proj):
    from eth_abi import encode
    from eth_utils import function_abi_to_4byte_selector

    art = proj.artifact("Bank")
    entry = next(e for e in art.abi if e.get("type") == "error")
    payload = function_abi_to_4byte_selector(entry) + encode(
        ["address"], ["0x" + "22" * 20]
    )
    assert "NotOwner" in decode_revert(payload, art.abi)
