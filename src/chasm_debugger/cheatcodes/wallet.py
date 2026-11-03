"""Key-derivation and wallet cheats: BIP-44 `deriveKey`, EIP-2098 `signCompact`,
`rememberKey`, and `createWallet`. All operate on private keys directly; none need VM state.
"""

from __future__ import annotations

from typing import Any

from eth_account import Account
from eth_keys import keys
from eth_utils import keccak, to_checksum_address

from .registry import CheatContext, _cheat

Account.enable_unaudited_hdwallet_features()

_DEFAULT_PATH = "m/44'/60'/0'/0/"


def _derive(mnemonic: str, path: str, index: int) -> int:
    acct = Account.from_mnemonic(mnemonic, account_path=f"{path}{index}")
    return int.from_bytes(acct.key, "big")


@_cheat(
    "deriveKey(string,uint32)",
    ret_types=["uint256"],
    family="deriveKey",
    doc="BIP-44 derive a key from a mnemonic",
)
def _derive_default(ctx: CheatContext) -> list[Any]:
    return [_derive(str(ctx.args[0]), _DEFAULT_PATH, int(ctx.args[1]))]


@_cheat(
    "deriveKey(string,string,uint32)",
    ret_types=["uint256"],
    family="deriveKey",
    doc="BIP-44 derive a key from a mnemonic",
)
def _derive_path(ctx: CheatContext) -> list[Any]:
    return [_derive(str(ctx.args[0]), str(ctx.args[1]), int(ctx.args[2]))]


@_cheat(
    "signCompact(uint256,bytes32)",
    ret_types=["bytes32", "bytes32"],
    doc="EIP-2098 compact sign a hash, returning (r, vs)",
)
def _sign_compact(ctx: CheatContext) -> list[Any]:
    pk = keys.PrivateKey(int(ctx.args[0]).to_bytes(32, "big"))
    sig = pk.sign_msg_hash(bytes(ctx.args[1]))
    y_parity = sig.v & 1
    vs = sig.s | (y_parity << 255)
    return [sig.r.to_bytes(32, "big"), vs.to_bytes(32, "big")]


@_cheat(
    "rememberKey(uint256)",
    ret_types=["address"],
    doc="return the address of a private key",
)
def _remember_key(ctx: CheatContext) -> list[Any]:
    pk = keys.PrivateKey(int(ctx.args[0]).to_bytes(32, "big"))
    return [pk.public_key.to_checksum_address()]


def _wallet_from_pk(pk_int: int) -> list[Any]:
    pk = keys.PrivateKey(pk_int.to_bytes(32, "big"))
    pub = pk.public_key.to_bytes()  # 64 bytes: X || Y
    addr = to_checksum_address(pk.public_key.to_canonical_address())
    return [
        addr,
        int.from_bytes(pub[:32], "big"),
        int.from_bytes(pub[32:], "big"),
        pk_int,
    ]


# Wallet is `struct { address addr; uint256 publicKeyX; uint256 publicKeyY; uint256
# privateKey; }`; a struct of static fields ABI-encodes identically to its members in order.
_WALLET_RET = ["address", "uint256", "uint256", "uint256"]


@_cheat(
    "createWallet(uint256)",
    ret_types=_WALLET_RET,
    family="createWallet",
    doc="build a Wallet from a key/name",
)
def _wallet_from_key(ctx: CheatContext) -> list[Any]:
    return _wallet_from_pk(int(ctx.args[0]))


@_cheat(
    "createWallet(string)",
    ret_types=_WALLET_RET,
    family="createWallet",
    doc="build a Wallet from a key/name",
)
def _wallet_from_name(ctx: CheatContext) -> list[Any]:
    pk = int.from_bytes(keccak(str(ctx.args[0]).encode()), "big")
    wallet = _wallet_from_pk(pk)
    ctx.cheats.labels[bytes.fromhex(wallet[0][2:])] = str(ctx.args[0])
    return wallet


@_cheat(
    "createWallet(uint256,string)",
    ret_types=_WALLET_RET,
    family="createWallet",
    doc="build a Wallet from a key/name",
)
def _wallet_from_key_named(ctx: CheatContext) -> list[Any]:
    wallet = _wallet_from_pk(int(ctx.args[0]))
    ctx.cheats.labels[bytes.fromhex(wallet[0][2:])] = str(ctx.args[1])
    return wallet
