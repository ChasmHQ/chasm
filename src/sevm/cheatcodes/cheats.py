"""The implemented cheatcodes: environment, account state, identity, keys, labelling.

Everything else declared in forge-std's `Vm.sol` reverts with a clear "unimplemented"
message rather than silently doing nothing.
"""

from __future__ import annotations

from typing import Any

from eth_keys import keys
from eth_utils import to_checksum_address

from .registry import CheatContext, CheatError, Prank, _addr, _cheat

# ---- environment ----------------------------------------------------------


@_cheat("warp(uint256)", doc="set block.timestamp")
def _warp(ctx: CheatContext) -> None:
    ctx.state.execution_context._timestamp = int(ctx.args[0])


@_cheat("roll(uint256)", doc="set block.number")
def _roll(ctx: CheatContext) -> None:
    ctx.state.execution_context._block_number = int(ctx.args[0])


@_cheat("fee(uint256)", doc="set block.basefee")
def _fee(ctx: CheatContext) -> None:
    ctx.state.execution_context._base_fee_per_gas = int(ctx.args[0])


@_cheat("chainId(uint256)", doc="set the chain id")
def _chain_id(ctx: CheatContext) -> None:
    ctx.state.execution_context._chain_id = int(ctx.args[0])


@_cheat("coinbase(address)", doc="set block.coinbase")
def _coinbase(ctx: CheatContext) -> None:
    ctx.state.execution_context._coinbase = _addr(ctx.args[0])


# ---- account state --------------------------------------------------------


@_cheat("deal(address,uint256)", doc="set an account's balance")
def _deal(ctx: CheatContext) -> None:
    ctx.state.set_balance(_addr(ctx.args[0]), int(ctx.args[1]))


@_cheat("etch(address,bytes)", doc="replace an account's code")
def _etch(ctx: CheatContext) -> None:
    ctx.state.set_code(_addr(ctx.args[0]), bytes(ctx.args[1]))


@_cheat("store(address,bytes32,bytes32)", doc="write a raw storage slot of any account")
def _store(ctx: CheatContext) -> None:
    slot = int.from_bytes(ctx.args[1], "big")
    value = int.from_bytes(ctx.args[2], "big")
    ctx.state.set_storage(_addr(ctx.args[0]), slot, value)


@_cheat(
    "load(address,bytes32)",
    ret_types=["bytes32"],
    doc="read a raw storage slot of any account",
)
def _load(ctx: CheatContext) -> list[Any]:
    slot = int.from_bytes(ctx.args[1], "big")
    value = ctx.state.get_storage(_addr(ctx.args[0]), slot)
    return [int(value).to_bytes(32, "big")]


# ---- identity / prank -----------------------------------------------------


@_cheat("prank(address)", doc="rewrite msg.sender for the next call only")
def _prank(ctx: CheatContext) -> None:
    ctx.cheats.prank = Prank(
        caller=ctx.caller, new_sender=_addr(ctx.args[0]), persistent=False
    )


@_cheat("startPrank(address)", doc="rewrite msg.sender until stopPrank")
def _start_prank(ctx: CheatContext) -> None:
    ctx.cheats.prank = Prank(
        caller=ctx.caller, new_sender=_addr(ctx.args[0]), persistent=True
    )


@_cheat("stopPrank()", doc="end an active startPrank")
def _stop_prank(ctx: CheatContext) -> None:
    ctx.cheats.prank = None


# ---- keys / signing -------------------------------------------------------


@_cheat("addr(uint256)", ret_types=["address"], doc="the address of a private key")
def _addr_of(ctx: CheatContext) -> list[Any]:
    pk = keys.PrivateKey(int(ctx.args[0]).to_bytes(32, "big"))
    return [pk.public_key.to_checksum_address()]


@_cheat(
    "sign(uint256,bytes32)",
    ret_types=["uint8", "bytes32", "bytes32"],
    doc="sign a hash with a private key, returning (v, r, s)",
)
def _sign(ctx: CheatContext) -> list[Any]:
    pk = keys.PrivateKey(int(ctx.args[0]).to_bytes(32, "big"))
    sig = pk.sign_msg_hash(bytes(ctx.args[1]))
    return [sig.v + 27, sig.r.to_bytes(32, "big"), sig.s.to_bytes(32, "big")]


# ---- fuzzing / labelling --------------------------------------------------


@_cheat("assume(bool)", doc="reject a fuzz input that fails the condition")
def _assume(ctx: CheatContext) -> None:
    if not ctx.args[0]:
        raise CheatError("vm.assume rejected the input")


@_cheat("label(address,string)", doc="give an address a readable name")
def _label(ctx: CheatContext) -> None:
    ctx.cheats.labels[_addr(ctx.args[0])] = ctx.args[1]


# ---- prank: sender + origin, and the delegatecall/origin overloads -----------------------


def _prank_spec(sig: str, persistent: bool, *, has_origin: bool, has_bool: bool) -> None:
    @_cheat(sig, family="prank", doc="rewrite msg.sender (and optionally tx.origin)")
    def _fn(ctx: CheatContext) -> None:
        new_sender = _addr(ctx.args[0])
        new_origin = _addr(ctx.args[1]) if has_origin else None
        delegate = bool(ctx.args[-1]) if has_bool else False
        ctx.cheats.prank = Prank(
            caller=ctx.caller,
            new_sender=new_sender,
            persistent=persistent,
            new_origin=new_origin,
            delegate=delegate,
        )


for _persistent, _base in ((False, "prank"), (True, "startPrank")):
    _prank_spec(f"{_base}(address,bool)", _persistent, has_origin=False, has_bool=True)
    _prank_spec(f"{_base}(address,address)", _persistent, has_origin=True, has_bool=False)
    _prank_spec(
        f"{_base}(address,address,bool)", _persistent, has_origin=True, has_bool=True
    )


@_cheat(
    "getLabel(address)", ret_types=["string"], doc="the label set for an address, if any"
)
def _get_label(ctx: CheatContext) -> list[Any]:
    addr = _addr(ctx.args[0])
    label = ctx.cheats.labels.get(addr)
    # forge's fallback prints the EIP-55 form, not eth_abi's lowercased decode.
    return [label if label is not None else f"unlabeled:{to_checksum_address(addr)}"]


# ---- nonce ----------------------------------------------------------------------------


@_cheat("getNonce(address)", ret_types=["uint64"], doc="an account's nonce")
def _get_nonce(ctx: CheatContext) -> list[Any]:
    return [ctx.state.get_nonce(_addr(ctx.args[0]))]


@_cheat("setNonce(address,uint64)", doc="set an account's nonce (must not lower it)")
def _set_nonce(ctx: CheatContext) -> None:
    addr = _addr(ctx.args[0])
    new = int(ctx.args[1])
    if new < ctx.state.get_nonce(addr):
        raise CheatError(
            "setNonce: new nonce must be >= current nonce; use setNonceUnsafe"
        )
    ctx.state.set_nonce(addr, new)


@_cheat("setNonceUnsafe(address,uint64)", doc="set an account's nonce with no check")
def _set_nonce_unsafe(ctx: CheatContext) -> None:
    ctx.state.set_nonce(_addr(ctx.args[0]), int(ctx.args[1]))


@_cheat("resetNonce(address)", doc="reset an account's nonce to zero")
def _reset_nonce(ctx: CheatContext) -> None:
    ctx.state.set_nonce(_addr(ctx.args[0]), 0)


# ---- more block environment -------------------------------------------------------------


@_cheat("prevrandao(bytes32)", doc="set block.prevrandao")
def _prevrandao_b32(ctx: CheatContext) -> None:
    ctx.state.execution_context._mix_hash = bytes(ctx.args[0])


@_cheat("prevrandao(uint256)", doc="set block.prevrandao")
def _prevrandao_uint(ctx: CheatContext) -> None:
    ctx.state.execution_context._mix_hash = int(ctx.args[0]).to_bytes(32, "big")


@_cheat("difficulty(uint256)", doc="set block.difficulty (pre-merge)")
def _difficulty(ctx: CheatContext) -> None:
    ctx.state.execution_context._difficulty = int(ctx.args[0])


@_cheat("getBlockNumber()", ret_types=["uint256"], doc="current block.number")
def _get_block_number(ctx: CheatContext) -> list[Any]:
    return [ctx.state.execution_context.block_number]


@_cheat("getBlockTimestamp()", ret_types=["uint256"], doc="current block.timestamp")
def _get_block_timestamp(ctx: CheatContext) -> list[Any]:
    return [ctx.state.execution_context.timestamp]


@_cheat("getChainId()", ret_types=["uint256"], doc="current chain id")
def _get_chain_id(ctx: CheatContext) -> list[Any]:
    return [ctx.state.execution_context.chain_id]


# ---- access list ------------------------------------------------------------------------


@_cheat("warmSlot(address,bytes32)", doc="mark a storage slot warm (EIP-2929)")
def _warm_slot(ctx: CheatContext) -> None:
    slot = int.from_bytes(bytes(ctx.args[1]), "big")
    ctx.state.mark_storage_warm(_addr(ctx.args[0]), slot)


# ---- gas metering / control: no-ops that keep a test running ----------------------------
# sevm meters and reports gas itself; these forge knobs only affect gas *snapshots*, which
# sevm does not produce, so accepting them as no-ops lets a test that toggles them run.


@_cheat("pauseGasMetering()", doc="no-op (sevm does not take gas snapshots)")
def _pause_gas(ctx: CheatContext) -> None:
    return None


@_cheat("resumeGasMetering()", doc="no-op (sevm does not take gas snapshots)")
def _resume_gas(ctx: CheatContext) -> None:
    return None


@_cheat("resetGasMetering()", doc="no-op (sevm does not take gas snapshots)")
def _reset_gas(ctx: CheatContext) -> None:
    return None


@_cheat("skip(bool)", doc="mark a fuzz/test as skipped (accepted, no-op)")
def _skip(ctx: CheatContext) -> None:
    return None


@_cheat("skip(bool,string)", doc="mark a fuzz/test as skipped (accepted, no-op)")
def _skip_reason(ctx: CheatContext) -> None:
    return None


# ---- randomness (seeded, reproducible; change it with setSeed) ---------------------------


@_cheat("setSeed(uint256)", doc="reseed vm.random*")
def _set_seed(ctx: CheatContext) -> None:
    ctx.cheats.reseed(int(ctx.args[0]))


@_cheat("randomUint()", ret_types=["uint256"], family="random", doc="a random value")
def _random_uint(ctx: CheatContext) -> list[Any]:
    return [ctx.cheats.rng.getrandbits(256)]


def _bits(name: str, value: Any) -> int:
    """A `random*(bits)` width. Anything over 256 would encode out of range, so it is
    refused here rather than escaping the cheat engine as an eth_abi error."""
    bits = int(value)
    if bits > 256:
        raise CheatError(f"vm.{name}: number of bits cannot exceed 256")
    return bits


@_cheat(
    "randomUint(uint256)", ret_types=["uint256"], family="random", doc="a random value"
)
def _random_uint_bits(ctx: CheatContext) -> list[Any]:
    bits = _bits("randomUint", ctx.args[0])
    return [ctx.cheats.rng.getrandbits(bits) if bits else 0]


@_cheat(
    "randomUint(uint256,uint256)",
    ret_types=["uint256"],
    family="random",
    doc="a random value",
)
def _random_uint_range(ctx: CheatContext) -> list[Any]:
    lo, hi = int(ctx.args[0]), int(ctx.args[1])
    if lo > hi:
        raise CheatError("vm.randomUint: min must be less than or equal to max")
    return [ctx.cheats.rng.randint(lo, hi)]


@_cheat("randomInt()", ret_types=["int256"], family="random", doc="a random value")
def _random_int(ctx: CheatContext) -> list[Any]:
    return [ctx.cheats.rng.getrandbits(256) - (1 << 255)]


@_cheat("randomInt(uint256)", ret_types=["int256"], family="random", doc="a random value")
def _random_int_bits(ctx: CheatContext) -> list[Any]:
    bits = _bits("randomInt", ctx.args[0])
    val = ctx.cheats.rng.getrandbits(bits) if bits else 0
    if bits and val >= (1 << (bits - 1)):
        val -= 1 << bits
    return [val]


@_cheat("randomAddress()", ret_types=["address"], family="random", doc="a random value")
def _random_address(ctx: CheatContext) -> list[Any]:
    return [to_checksum_address(ctx.cheats.rng.getrandbits(160).to_bytes(20, "big"))]


@_cheat("randomBool()", ret_types=["bool"], family="random", doc="a random value")
def _random_bool(ctx: CheatContext) -> list[Any]:
    return [bool(ctx.cheats.rng.getrandbits(1))]


@_cheat(
    "randomBytes(uint256)", ret_types=["bytes"], family="random", doc="a random value"
)
def _random_bytes(ctx: CheatContext) -> list[Any]:
    n = int(ctx.args[0])
    return [bytes(ctx.cheats.rng.getrandbits(8) for _ in range(n))]


@_cheat("randomBytes4()", ret_types=["bytes4"], family="random", doc="a random value")
def _random_bytes4(ctx: CheatContext) -> list[Any]:
    return [ctx.cheats.rng.getrandbits(32).to_bytes(4, "big")]


@_cheat("randomBytes8()", ret_types=["bytes8"], family="random", doc="a random value")
def _random_bytes8(ctx: CheatContext) -> list[Any]:
    return [ctx.cheats.rng.getrandbits(64).to_bytes(8, "big")]
