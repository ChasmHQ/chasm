"""Every registered cheatcode, exercised once.

Expected values here are not invented: each was taken from a run of the same call under
real `forge test` against forge-std, so this file is sevm's differential record of Foundry
behaviour. `test_every_cheat_is_exercised` fails if a cheat is added without a case, which
is what keeps that record complete.

`apply_cheat` is called directly rather than through Solidity so the array overloads are
reachable at all (the interactive prompt heuristic never picks those), and arguments are
ABI-encoded from each spec's own declared types.
"""

from __future__ import annotations

import os

import pytest
from eth_abi import decode as abi_decode
from eth_abi import encode as abi_encode
from eth_utils import function_signature_to_4byte_selector

from sevm.cheatcodes import CheatError, CheatState, all_specs, apply_cheat

SPECS = {spec.signature: spec for spec in all_specs()}

ALICE = "0x" + "11" * 20
BOB = "0x" + "22" * 20
ALICE_RAW = bytes.fromhex("11" * 20)
CALLER = bytes.fromhex("cc" * 20)

# Private key 1 and its identity, the values forge prints for `vm.addr(1)`.
KEY1_ADDR = "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
KEY1_X = 0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798
KEY1_Y = 0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8

MNEMONIC = "test test test test test test test test test test test junk"
MAX_UINT = (1 << 256) - 1
B32_ONE = (1).to_bytes(32, "big")


def call(signature: str, args: list, *, state=None, cheats=None):
    """Run one cheat through the registry, ABI-encoded by its own declared types."""
    spec = SPECS[signature]
    body = abi_encode(spec.arg_types, args) if spec.arg_types else b""
    data = function_signature_to_4byte_selector(signature) + body
    out = apply_cheat(cheats if cheats is not None else CheatState(), state, data, CALLER)
    return abi_decode(spec.ret_types, out) if spec.ret_types else None


# ==================================================================
# pure cheats: signature -> (args, expected decoded tuple)
# ==================================================================

PURE_CASES: list[tuple[str, list, tuple | None]] = [
    # ---- keys and signing ----
    ("addr(uint256)", [1], (KEY1_ADDR,)),
    ("rememberKey(uint256)", [1], (KEY1_ADDR,)),
    (
        "deriveKey(string,uint32)",
        [MNEMONIC, 0],
        (0xAC0974BEC39A17E36BA4A6B4D238FF944BACB478CBED5EFCAE784D7BF4F2FF80,),
    ),
    (
        "deriveKey(string,string,uint32)",
        [MNEMONIC, "m/44'/60'/0'/0/", 1],
        (0x59C6995E998F97A5A0044966F0945389DC9E86DAE88C7A8412F4603B6B78690D,),
    ),
    ("createWallet(uint256)", [1], (KEY1_ADDR, KEY1_X, KEY1_Y, 1)),
    ("createWallet(uint256,string)", [1, "one"], (KEY1_ADDR, KEY1_X, KEY1_Y, 1)),
    # ---- accepted no-ops ----
    ("assume(bool)", [True], None),
    ("skip(bool)", [True], None),
    ("skip(bool,string)", [True, "not now"], None),
    ("pauseGasMetering()", [], None),
    ("resumeGasMetering()", [], None),
    ("resetGasMetering()", [], None),
    # ---- toString ----
    ("toString(uint256)", [255], ("255",)),
    ("toString(int256)", [-3], ("-3",)),
    ("toString(bool)", [True], ("true",)),
    ("toString(address)", [ALICE], (ALICE,)),
    ("toString(bytes32)", [B32_ONE], ("0x" + "00" * 31 + "01",)),
    ("toString(bytes)", [b"\xde\xad"], ("0xdead",)),
    # ---- parse ----
    ("parseUint(string)", ["255"], (255,)),
    ("parseInt(string)", ["-42"], (-42,)),
    ("parseBool(string)", ["TRUE"], (True,)),
    ("parseAddress(string)", [ALICE], (ALICE,)),
    ("parseBytes(string)", ["0xdeadbeef"], (b"\xde\xad\xbe\xef",)),
    ("parseBytes32(string)", ["0x" + "00" * 31 + "01"], (B32_ONE,)),
    # ---- base64: forge keeps the padding on both variants ----
    ("toBase64(bytes)", [b"hello"], ("aGVsbG8=",)),
    ("toBase64(string)", ["hello"], ("aGVsbG8=",)),
    ("toBase64URL(bytes)", [b"\xff\xff"], ("__8=",)),
    ("toBase64URL(string)", ["hello"], ("aGVsbG8=",)),
    # ---- string ops ----
    ("toUppercase(string)", ["abc"], ("ABC",)),
    ("toLowercase(string)", ["ABC"], ("abc",)),
    ("trim(string)", ["  hi  "], ("hi",)),
    ("replace(string,string,string)", ["a-b", "-", "+"], ("a+b",)),
    ("contains(string,string)", ["hello", "ell"], (True,)),
    ("split(string,string)", ["a,b,c", ","], (("a", "b", "c"),)),
    # A byte offset, so a multi-byte character counts for more than one.
    ("indexOf(string,string)", ["aé b", "b"], (4,)),
    # ---- address computation ----
    (
        "computeCreateAddress(address,uint256)",
        ["0x" + "00" * 20, 0],
        ("0xbd770416a3345f91e4b34576cb804a576fa48eb1",),
    ),
    (
        "computeCreate2Address(bytes32,bytes32)",
        [b"\x00" * 32, b"\x00" * 32],
        ("0x778a4590f20db0c23cb7c1befc8da04549f2aa95",),
    ),
    (
        "computeCreate2Address(bytes32,bytes32,address)",
        [b"\x00" * 32, b"\x00" * 32, "0x4e59b44847b379578588920ca78fbf26c0b4956c"],
        ("0x778a4590f20db0c23cb7c1befc8da04549f2aa95",),
    ),
]


@pytest.mark.parametrize(
    "signature,args,expected", PURE_CASES, ids=[c[0] for c in PURE_CASES]
)
def test_pure_cheats(signature, args, expected):
    assert call(signature, args) == expected


# ==================================================================
# the process environment
# ==================================================================

ENV_VALUES = {
    "SEVM_UINT": "42",
    "SEVM_UINT_HEX": "0xff",
    "SEVM_INT": "-5",
    "SEVM_BOOL": "true",
    "SEVM_BOOL_ONE": "1",
    "SEVM_ADDR": ALICE,
    "SEVM_B32": "0x" + "00" * 28 + "deadbeef",
    "SEVM_BYTES": "0xdeadbeef",
    "SEVM_STR": "hello",
    "SEVM_UINTS": "1,2,3",
    "SEVM_INTS": "-1,2",
    "SEVM_BOOLS": "true,false",
    "SEVM_ADDRS": f"{ALICE},{BOB}",
    "SEVM_B32S": "0x" + "00" * 31 + "01,0x" + "00" * 31 + "02",
    "SEVM_BYTESS": "0xdead,0xbeef",
    "SEVM_STRS": "a,b",
}

ENV_CASES: list[tuple[str, list, tuple | None]] = [
    ("envUint(string)", ["SEVM_UINT"], (42,)),
    ("envInt(string)", ["SEVM_INT"], (-5,)),
    ("envBool(string)", ["SEVM_BOOL"], (True,)),
    ("envAddress(string)", ["SEVM_ADDR"], (ALICE,)),
    ("envBytes32(string)", ["SEVM_B32"], (bytes.fromhex("00" * 28 + "deadbeef"),)),
    ("envBytes(string)", ["SEVM_BYTES"], (b"\xde\xad\xbe\xef",)),
    ("envString(string)", ["SEVM_STR"], ("hello",)),
    ("envExists(string)", ["SEVM_UINT"], (True,)),
    ("envUint(string,string)", ["SEVM_UINTS", ","], ((1, 2, 3),)),
    ("envInt(string,string)", ["SEVM_INTS", ","], ((-1, 2),)),
    ("envBool(string,string)", ["SEVM_BOOLS", ","], ((True, False),)),
    ("envAddress(string,string)", ["SEVM_ADDRS", ","], ((ALICE, BOB),)),
    (
        "envBytes32(string,string)",
        ["SEVM_B32S", ","],
        ((B32_ONE, (2).to_bytes(32, "big")),),
    ),
    ("envBytes(string,string)", ["SEVM_BYTESS", ","], ((b"\xde\xad", b"\xbe\xef"),)),
    ("envString(string,string)", ["SEVM_STRS", ","], (("a", "b"),)),
    # envOr, on a miss: the caller's default comes straight back
    ("envOr(string,uint256)", ["SEVM_MISSING", 7], (7,)),
    ("envOr(string,int256)", ["SEVM_MISSING", -7], (-7,)),
    ("envOr(string,bool)", ["SEVM_MISSING", True], (True,)),
    ("envOr(string,address)", ["SEVM_MISSING", ALICE], (ALICE,)),
    ("envOr(string,bytes32)", ["SEVM_MISSING", B32_ONE], (B32_ONE,)),
    ("envOr(string,bytes)", ["SEVM_MISSING", b"\x01"], (b"\x01",)),
    ("envOr(string,string)", ["SEVM_MISSING", "fallback"], ("fallback",)),
    ("envOr(string,string,uint256[])", ["SEVM_MISSING", ",", [4, 5]], ((4, 5),)),
    ("envOr(string,string,int256[])", ["SEVM_MISSING", ",", [-4]], ((-4,),)),
    ("envOr(string,string,bool[])", ["SEVM_MISSING", ",", [True]], ((True,),)),
    ("envOr(string,string,address[])", ["SEVM_MISSING", ",", [ALICE]], ((ALICE,),)),
    ("envOr(string,string,bytes32[])", ["SEVM_MISSING", ",", [B32_ONE]], ((B32_ONE,),)),
    ("envOr(string,string,bytes[])", ["SEVM_MISSING", ",", [b"\xaa"]], ((b"\xaa",),)),
    ("envOr(string,string,string[])", ["SEVM_MISSING", ",", ["z"]], (("z",),)),
]


@pytest.fixture
def env(monkeypatch):
    for name, value in ENV_VALUES.items():
        monkeypatch.setenv(name, value)
    monkeypatch.delenv("SEVM_MISSING", raising=False)


@pytest.mark.parametrize(
    "signature,args,expected", ENV_CASES, ids=[c[0] + str(c[1][:1]) for c in ENV_CASES]
)
def test_env_cheats(env, signature, args, expected):
    assert call(signature, args) == expected


def test_env_or_prefers_the_variable_over_the_default(env):
    assert call("envOr(string,uint256)", ["SEVM_UINT", 7]) == (42,)
    assert call("envOr(string,string,uint256[])", ["SEVM_UINTS", ",", [4]]) == (
        (1, 2, 3),
    )


def test_env_exists_is_false_for_an_unset_variable(env):
    assert call("envExists(string)", ["SEVM_MISSING"]) == (False,)


def test_env_missing_variable_reverts(env):
    with pytest.raises(CheatError, match="not found"):
        call("envUint(string)", ["SEVM_MISSING"])


def test_set_env_writes_the_process_environment(monkeypatch):
    monkeypatch.delenv("SEVM_SET_AT_RUNTIME", raising=False)
    call("setEnv(string,string)", ["SEVM_SET_AT_RUNTIME", "123"])
    try:
        assert os.environ["SEVM_SET_AT_RUNTIME"] == "123"
        assert call("envUint(string)", ["SEVM_SET_AT_RUNTIME"]) == (123,)
    finally:
        os.environ.pop("SEVM_SET_AT_RUNTIME", None)


def test_env_bool_accepts_one_and_zero(env):
    # forge's parser takes 1/0 as well as true/false.
    assert call("envBool(string)", ["SEVM_BOOL_ONE"]) == (True,)


def test_env_uint_accepts_hex(env):
    assert call("envUint(string)", ["SEVM_UINT_HEX"]) == (255,)


@pytest.mark.parametrize(
    "signature,args",
    [
        ("parseBytes32(string)", ["0xdead"]),
        ("envBytes32(string)", ["SEVM_SHORT"]),
    ],
)
def test_short_bytes32_is_refused_not_padded(monkeypatch, signature, args):
    # forge rejects it: a short literal pads left as an integer and right as a bytes32,
    # so guessing would be silently and plausibly wrong.
    monkeypatch.setenv("SEVM_SHORT", "0xdead")
    with pytest.raises(CheatError, match="32 bytes"):
        call(signature, args)


# ==================================================================
# labels and pranks: bookkeeping on the CheatState
# ==================================================================


def test_label_and_get_label():
    cheats = CheatState()
    call("label(address,string)", [ALICE, "alice"], cheats=cheats)
    assert cheats.labels[ALICE_RAW] == "alice"
    assert call("getLabel(address)", [ALICE], cheats=cheats) == ("alice",)


def test_get_label_falls_back_to_the_checksummed_address():
    assert call("getLabel(address)", ["0x" + "00" * 18 + "0b0b"]) == (
        "unlabeled:0x0000000000000000000000000000000000000B0b",
    )


def test_create_wallet_from_a_name_labels_the_address():
    cheats = CheatState()
    addr, _x, _y, key = call("createWallet(string)", ["alice-wallet"], cheats=cheats)
    assert call("getLabel(address)", [addr], cheats=cheats) == ("alice-wallet",)
    assert call("addr(uint256)", [key]) == (addr,)


def test_create_wallet_from_a_key_and_name_labels_it():
    cheats = CheatState()
    addr, *_ = call("createWallet(uint256,string)", [1, "one"], cheats=cheats)
    assert call("getLabel(address)", [addr], cheats=cheats) == ("one",)


PRANK_CASES = [
    ("prank(address)", [ALICE], False, None, False),
    ("prank(address,bool)", [ALICE, True], False, None, True),
    ("prank(address,address)", [ALICE, BOB], False, BOB, False),
    ("prank(address,address,bool)", [ALICE, BOB, True], False, BOB, True),
    ("startPrank(address)", [ALICE], True, None, False),
    ("startPrank(address,bool)", [ALICE, True], True, None, True),
    ("startPrank(address,address)", [ALICE, BOB], True, BOB, False),
    ("startPrank(address,address,bool)", [ALICE, BOB, True], True, BOB, True),
]


@pytest.mark.parametrize(
    "signature,args,persistent,origin,delegate",
    PRANK_CASES,
    ids=[c[0] for c in PRANK_CASES],
)
def test_prank_overloads_record_the_right_prank(
    signature, args, persistent, origin, delegate
):
    cheats = CheatState()
    call(signature, args, cheats=cheats)
    prank = cheats.prank
    assert prank.new_sender == ALICE_RAW
    assert prank.caller == CALLER
    assert prank.persistent is persistent
    assert prank.delegate is delegate
    assert prank.new_origin == (bytes.fromhex("22" * 20) if origin else None)


def test_stop_prank_clears_it():
    cheats = CheatState()
    call("startPrank(address)", [ALICE], cheats=cheats)
    call("stopPrank()", [], cheats=cheats)
    assert cheats.prank is None


def test_assume_rejects_a_false_condition():
    with pytest.raises(CheatError, match="assume"):
        call("assume(bool)", [False])


# ==================================================================
# randomness: seeded, so a debugger re-run reproduces the values
# ==================================================================

RANDOM_SIGNATURES = [
    ("randomUint()", []),
    ("randomUint(uint256)", [8]),
    ("randomUint(uint256,uint256)", [10, 12]),
    ("randomInt()", []),
    ("randomInt(uint256)", [8]),
    ("randomAddress()", []),
    ("randomBool()", []),
    ("randomBytes(uint256)", [4]),
    ("randomBytes4()", []),
    ("randomBytes8()", []),
]


@pytest.mark.parametrize(
    "signature,args", RANDOM_SIGNATURES, ids=[c[0] for c in RANDOM_SIGNATURES]
)
def test_random_is_reproducible_from_the_default_seed(signature, args):
    assert call(signature, args) == call(signature, args)


def test_random_values_stay_in_range():
    assert call("randomUint(uint256)", [0]) == (0,)
    assert 0 <= call("randomUint(uint256)", [8])[0] <= 255
    assert -128 <= call("randomInt(uint256)", [8])[0] <= 127
    assert len(call("randomBytes(uint256)", [4])[0]) == 4
    assert call("randomUint(uint256,uint256)", [5, 5]) == (5,)
    for _ in range(20):
        assert 10 <= call("randomUint(uint256,uint256)", [10, 12])[0] <= 12


@pytest.mark.parametrize("signature", ["randomUint(uint256)", "randomInt(uint256)"])
def test_random_refuses_more_than_256_bits(signature):
    # Not a guard for its own sake: the oversized value would leave the cheat engine as an
    # eth_abi encoding error, outside the revert path a test can catch.
    with pytest.raises(CheatError, match="cannot exceed 256"):
        call(signature, [300])


def test_random_uint_range_must_be_ordered():
    with pytest.raises(CheatError, match="min must be less than or equal to max"):
        call("randomUint(uint256,uint256)", [9, 3])


def test_set_seed_changes_the_stream():
    first, second = CheatState(), CheatState()
    call("setSeed(uint256)", [1], cheats=first)
    call("setSeed(uint256)", [2], cheats=second)
    assert call("randomUint()", [], cheats=first) != call(
        "randomUint()", [], cheats=second
    )


# ==================================================================
# signing
# ==================================================================


def test_sign_returns_a_recoverable_signature():
    from eth_keys import keys

    digest = bytes.fromhex("02" * 32)
    v, r, s = call("sign(uint256,bytes32)", [1, digest])
    assert v in (27, 28)
    signature = keys.Signature(
        vrs=(v - 27, int.from_bytes(r, "big"), int.from_bytes(s, "big"))
    )
    recovered = signature.recover_public_key_from_msg_hash(digest)
    assert recovered.to_checksum_address().lower() == KEY1_ADDR


def test_sign_compact_packs_the_parity_into_s():
    digest = bytes.fromhex("02" * 32)
    v, r, s = call("sign(uint256,bytes32)", [1, digest])
    compact_r, vs = call("signCompact(uint256,bytes32)", [1, digest])
    assert compact_r == r
    assert int.from_bytes(vs, "big") == int.from_bytes(s, "big") | ((v - 27) << 255)


# ==================================================================
# cheats that read or write live VM state
# ==================================================================


@pytest.fixture
def state():
    """A live Py-EVM state, so the block-environment and account cheats are proven against
    the real object rather than a stand-in that would accept any attribute name."""
    from harness import make_web3

    return make_web3().provider.ethereum_tester.backend.chain.get_vm().state


def test_block_environment_cheats(state):
    call("warp(uint256)", [4242], state=state)
    assert state.execution_context.timestamp == 4242
    assert call("getBlockTimestamp()", [], state=state) == (4242,)

    call("roll(uint256)", [99], state=state)
    assert state.execution_context.block_number == 99
    assert call("getBlockNumber()", [], state=state) == (99,)

    call("chainId(uint256)", [31337], state=state)
    assert state.execution_context.chain_id == 31337
    assert call("getChainId()", [], state=state) == (31337,)

    call("fee(uint256)", [7 * 10**9], state=state)
    assert state.execution_context.base_fee_per_gas == 7 * 10**9

    call("coinbase(address)", [ALICE], state=state)
    assert state.execution_context.coinbase == ALICE_RAW

    call("difficulty(uint256)", [5], state=state)
    assert state.execution_context.difficulty == 5

    call("prevrandao(bytes32)", [B32_ONE], state=state)
    assert state.execution_context.mix_hash == B32_ONE
    call("prevrandao(uint256)", [2], state=state)
    assert state.execution_context.mix_hash == (2).to_bytes(32, "big")


def test_account_state_cheats(state):
    call("deal(address,uint256)", [ALICE, 10**18], state=state)
    assert state.get_balance(ALICE_RAW) == 10**18

    call("etch(address,bytes)", [ALICE, b"\x60\x00"], state=state)
    assert state.get_code(ALICE_RAW) == b"\x60\x00"

    call(
        "store(address,bytes32,bytes32)",
        [ALICE, B32_ONE, (7).to_bytes(32, "big")],
        state=state,
    )
    assert state.get_storage(ALICE_RAW, 1) == 7
    assert call("load(address,bytes32)", [ALICE, B32_ONE], state=state) == (
        (7).to_bytes(32, "big"),
    )

    call("warmSlot(address,bytes32)", [ALICE, B32_ONE], state=state)
    assert state.is_storage_warm(ALICE_RAW, 1)


def test_nonce_cheats(state):
    assert call("getNonce(address)", [ALICE], state=state) == (0,)
    call("setNonce(address,uint64)", [ALICE, 5], state=state)
    assert call("getNonce(address)", [ALICE], state=state) == (5,)
    # setNonce refuses to go backwards; setNonceUnsafe is the way down.
    with pytest.raises(CheatError, match="setNonceUnsafe"):
        call("setNonce(address,uint64)", [ALICE, 2], state=state)
    call("setNonceUnsafe(address,uint64)", [ALICE, 2], state=state)
    assert call("getNonce(address)", [ALICE], state=state) == (2,)
    call("resetNonce(address)", [ALICE], state=state)
    assert call("getNonce(address)", [ALICE], state=state) == (0,)


# ==================================================================
# the assert family, every overload
# ==================================================================

# One passing and one failing argument pair per asserted type.
_HOLDS: dict[str, tuple] = {
    "bool": (True, True),
    "uint256": (7, 7),
    "int256": (-7, -7),
    "address": (ALICE, ALICE),
    "bytes32": (B32_ONE, B32_ONE),
    "string": ("same", "same"),
    "bytes": (b"\x01", b"\x01"),
}
_DIFFERS: dict[str, tuple] = {
    "bool": (True, False),
    "uint256": (1, 2),
    "int256": (-1, -2),
    "address": (ALICE, BOB),
    "bytes32": (B32_ONE, (2).to_bytes(32, "big")),
    "string": ("left", "right"),
    "bytes": (b"\x01", b"\x02"),
}
# op -> (arguments that hold, arguments that do not), for the ordered comparisons.
_ORDERED = {
    "assertGt": ((2, 1), (1, 2)),
    "assertGe": ((1, 1), (1, 2)),
    "assertLt": ((1, 2), (2, 1)),
    "assertLe": ((1, 1), (2, 1)),
}


def _assert_arguments(spec) -> tuple[list, list]:
    """(arguments that must pass, arguments that must revert) for one assert overload.

    Built from the signature rather than listed, because the 116 overloads are themselves
    generated from an op x type matrix; a hand-written table would drift out of it.
    """
    name, types = spec.name, list(spec.arg_types)
    decimal = name.endswith("Decimal")
    stem = name.removesuffix("Decimal")
    base_arity = {
        "assertTrue": 1,
        "assertFalse": 1,
        "assertApproxEqAbs": 4 if decimal else 3,
        "assertApproxEqRel": 4 if decimal else 3,
    }.get(stem, 3 if decimal else 2)
    # A trailing `string` past the base arity is forge's custom error message, not a value.
    err = ["custom message"] if len(types) > base_arity else []
    decimals = [18] if decimal else []

    if stem in ("assertTrue", "assertFalse"):
        held = stem == "assertTrue"
        return [held, *err], [not held, *err]

    if stem in ("assertEq", "assertNotEq"):
        element = types[0].removesuffix("[]")
        same, differ = _HOLDS[element], _DIFFERS[element]
        if types[0].endswith("[]"):
            same = ([same[0]], [same[1]])
            differ = ([differ[0]], [differ[1]])
        holds, fails = (same, differ) if stem == "assertEq" else (differ, same)
        return [*holds, *decimals, *err], [*fails, *decimals, *err]

    if stem in ("assertApproxEqAbs", "assertApproxEqRel"):
        # A relative limit is 18-decimal fixed point, so 1e17 is 10%.
        limit = 10**17 if stem.endswith("Rel") else 5
        return [100, 100, limit, *decimals, *err], [100, 50, limit, *decimals, *err]

    holds, fails = _ORDERED[stem]
    return [*holds, *decimals, *err], [*fails, *decimals, *err]


ASSERT_SPECS = [spec for spec in all_specs() if spec.family == "assert"]


@pytest.mark.parametrize("spec", ASSERT_SPECS, ids=[s.signature for s in ASSERT_SPECS])
def test_every_assert_overload_passes_and_fails(spec):
    passing, failing = _assert_arguments(spec)
    call(spec.signature, passing)
    with pytest.raises(CheatError):
        call(spec.signature, failing)


def test_a_custom_message_replaces_forges_own():
    with pytest.raises(CheatError, match="balances drifted: 1 != 2"):
        call("assertEq(uint256,uint256,string)", [1, 2, "balances drifted"])


@pytest.mark.parametrize(
    "signature,args,message",
    [
        ("assertEq(uint256,uint256)", [1, 2], "assertion failed: 1 != 2"),
        ("assertNotEq(uint256,uint256)", [2, 2], "assertion failed: 2 == 2"),
        ("assertGt(uint256,uint256)", [1, 2], "assertion failed: 1 <= 2"),
        ("assertGe(uint256,uint256)", [1, 2], "assertion failed: 1 < 2"),
        ("assertLt(uint256,uint256)", [2, 1], "assertion failed: 2 >= 1"),
        ("assertLe(uint256,uint256)", [2, 1], "assertion failed: 2 > 1"),
        ("assertTrue(bool)", [False], "assertion failed"),
        (
            "assertEqDecimal(uint256,uint256,uint256)",
            [10**18, 2 * 10**18, 18],
            "assertion failed: 1.000000000000000000 != 2.000000000000000000",
        ),
        ("assertApproxEqAbs(uint256,uint256,uint256)", [100, 90, 5], "real delta: 10"),
        (
            "assertApproxEqRel(uint256,uint256,uint256)",
            [110, 100, 10**16],
            "real delta: 10.0000000000000000%",
        ),
        # A zero expectation has no relative delta; forge prints "undefined" rather than dividing.
        (
            "assertApproxEqRel(uint256,uint256,uint256)",
            [1, 0, 10**18],
            "real delta: undefined",
        ),
    ],
)
def test_assert_failures_print_forges_comparison(signature, args, message):
    with pytest.raises(CheatError) as exc:
        call(signature, args)
    assert message in str(exc.value)


def test_approx_eq_rel_holds_when_both_sides_are_zero():
    call("assertApproxEqRel(uint256,uint256,uint256)", [0, 0, 0])


# ==================================================================
# the coverage guard
# ==================================================================


def test_every_cheat_is_exercised():
    """A cheat added without a case in this file fails here rather than shipping untested."""
    covered = {signature for signature, *_ in PURE_CASES}
    covered |= {signature for signature, *_ in ENV_CASES}
    covered |= {signature for signature, *_ in PRANK_CASES}
    covered |= {signature for signature, _ in RANDOM_SIGNATURES}
    covered |= {spec.signature for spec in ASSERT_SPECS}
    covered |= {
        # exercised by the named tests above rather than by a table
        "setEnv(string,string)",
        "envExists(string)",
        "setSeed(uint256)",
        "stopPrank()",
        "label(address,string)",
        "getLabel(address)",
        "createWallet(string)",
        "sign(uint256,bytes32)",
        "signCompact(uint256,bytes32)",
        "warp(uint256)",
        "roll(uint256)",
        "fee(uint256)",
        "chainId(uint256)",
        "coinbase(address)",
        "difficulty(uint256)",
        "prevrandao(bytes32)",
        "prevrandao(uint256)",
        "getBlockNumber()",
        "getBlockTimestamp()",
        "getChainId()",
        "deal(address,uint256)",
        "etch(address,bytes)",
        "store(address,bytes32,bytes32)",
        "load(address,bytes32)",
        "warmSlot(address,bytes32)",
        "getNonce(address)",
        "setNonce(address,uint64)",
        "setNonceUnsafe(address,uint64)",
        "resetNonce(address)",
    }
    assert set(SPECS) - covered == set()
