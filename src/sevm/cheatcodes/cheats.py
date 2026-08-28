"""The implemented cheatcodes: environment, account state, identity, keys, labelling.

Everything else declared in forge-std's `Vm.sol` reverts with a clear "unimplemented"
message rather than silently doing nothing.
"""

from __future__ import annotations

from typing import Any

from eth_keys import keys

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
