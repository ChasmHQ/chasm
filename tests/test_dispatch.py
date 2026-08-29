"""The dispatcher read back out of bytecode, and the `sig` / `info address` commands.

The invariant worth guarding is that the internal address is the *implementation*, not
the wrapper: every selector's answer has to land on the line that declares the function,
and the VM has to actually stop there when you break on it.
"""

from __future__ import annotations

from eth_utils import keccak
from harness import line_of

from sevm.disasm import Disassembly
from sevm.dispatch import entries, find_selector
from sevm.srcmap import PcMap, build_line_indexes

DEPOSIT = 0xD0E30DB0


def _routes(proj, name):
    """(signature, entry, source line text) for every function of one contract."""
    art = proj.artifact(name)
    dis = Disassembly(art.deployed_bytecode)
    pcmap = PcMap(
        art.deployed_bytecode,
        art.deployed_source_map,
        build_line_indexes(proj.sources.values()),
    )
    for signature, selector in sorted(art.method_identifiers.items()):
        entry = find_selector(dis, int(selector, 16))
        assert entry is not None, f"{name}.{signature} is not in the dispatcher"
        loc = pcmap.at(entry.entry_pc)
        source = proj.source_by_id(loc.file_id).text.split("\n") if loc else []
        text = source[loc.line - 1] if loc and 0 < loc.line <= len(source) else ""
        yield signature, entry, text


def test_every_selector_routes_to_its_own_declaration(proj):
    """The end of the route is the implementation: the line that declares the function.

    Reaching only the wrapper would put every function on its own dispatcher stub, and a
    getter on none, so naming the declaration is what tells the two apart.
    """
    for name in ("Bank", "Callee", "Vault", "Locals"):
        for signature, entry, text in _routes(proj, name):
            base = signature.split("(")[0]
            assert entry.internal_pc is not None, (
                f"{name}.{signature} stopped at the wrapper"
            )
            assert entry.internal_pc != entry.wrapper_pc
            assert base in text, f"{name}.{signature} routed to {text!r}"


def test_the_payable_guard_is_not_mistaken_for_the_body(proj):
    """A non-payable function opens with a CALLVALUE check over a revert stub.

    Reading that stub's REVERT as the end of the wrapper loses the call entirely, which
    is how a scan written for `deposit()` reports nothing for `withdraw(uint256)`.
    """
    art = proj.artifact("Bank")
    dis = Disassembly(art.deployed_bytecode)
    withdraw = find_selector(dis, int(art.method_identifiers["withdraw(uint256)"], 16))
    assert withdraw is not None and withdraw.internal_pc is not None
    guard = dis.instructions[dis.index_of(withdraw.wrapper_pc) + 1]
    assert guard.mnemonic == "CALLVALUE"


def test_the_listing_matches_the_abi(proj):
    """Scanning for the comparison shape must not pick up `PUSH1 0x01; DUP2; EQ`.

    Bank's string helpers contain exactly that, four instructions from a JUMPI, so a
    width-agnostic scan reports a sixteenth function this contract does not have.
    """
    for name in ("Bank", "Callee", "Vault", "Locals"):
        art = proj.artifact(name)
        found = {entry.selector for entry in entries(Disassembly(art.deployed_bytecode))}
        assert found == {int(sel, 16) for sel in art.method_identifiers.values()}


def test_breaking_on_the_internal_address_stops_there(deposit_debugger, proj):
    """The address is real: the VM arrives at it, in the body, not in the wrapper."""
    dbg = deposit_debugger
    entry = find_selector(Disassembly(proj.artifact("Bank").deployed_bytecode), DEPOSIT)
    assert dbg.run(f"b *0x{entry.internal_pc:x}").ok
    assert dbg.run("c").resumed
    assert dbg.snap.pc == entry.internal_pc
    assert dbg.snap.line == line_of(proj, "function deposit() public payable {")


def test_info_address_reports_the_whole_route(deposit_debugger):
    result = deposit_debugger.run("info address withdraw")
    body = "\n".join(result.lines)
    assert result.ok
    assert "Bank.withdraw(uint256)" in body
    assert "0x2e1a7d4d" in body
    assert "0x0544" in body and "Bank.sol:" in body


def test_info_address_takes_a_signature_or_a_selector(deposit_debugger):
    for target in ("withdraw(uint256)", "0x2e1a7d4d", "Bank.withdraw"):
        assert "0x0544" in "\n".join(deposit_debugger.run(f"info address {target}").lines)


def test_info_address_reaches_another_contract(deposit_debugger):
    """`unsafeStore` is Vault's, and the session is stopped in Bank."""
    body = "\n".join(deposit_debugger.run("info address unsafeStore").lines)
    assert "Vault.unsafeStore(uint256,uint256)" in body and "Vault.sol:" in body


def test_info_address_says_when_a_selector_is_not_dispatched(deposit_debugger):
    result = deposit_debugger.run("info address transfer(address,uint256)")
    assert result.ok and "fallback" in "\n".join(result.lines)


def test_info_address_lists_every_selector(deposit_debugger, proj):
    lines = deposit_debugger.run("info address").lines
    assert len(lines) == len(proj.artifact("Bank").method_identifiers) + 1
    assert all("wrapper" in line for line in lines[1:])


def test_sig_resolves_a_name_to_its_signature_and_selector(deposit_debugger):
    assert deposit_debugger.run("sig withdraw").lines == [
        "[bold]withdraw(uint256)[/bold]  [cyan]0x2e1a7d4d[/cyan]"
    ]


def test_sig_hashes_a_signature_this_abi_does_not_declare(deposit_debugger):
    body = "\n".join(deposit_debugger.run("sig transfer(address,uint256)").lines)
    assert (
        f"0x{int.from_bytes(keccak(text='transfer(address,uint256)')[:4], 'big'):08x}"
        in body
    )
    assert "not in this ABI" in body


def test_sig_looks_a_selector_back_up(deposit_debugger):
    assert "withdraw(uint256)" in "\n".join(deposit_debugger.run("sig 0x2e1a7d4d").lines)
    assert "no function" in "\n".join(deposit_debugger.run("sig 0xdeadbeef").lines)


def test_sig_lists_the_contract_and_is_aliased(deposit_debugger, proj):
    listing = deposit_debugger.run("sig")
    assert len(listing.lines) == len(proj.artifact("Bank").method_identifiers) + 1
    assert (
        deposit_debugger.run("selector owner").lines
        == deposit_debugger.run("sig owner").lines
    )


def test_an_unknown_name_says_how_to_ask_anyway(deposit_debugger):
    result = deposit_debugger.run("info address nosuchthing")
    assert not result.ok and "info functions" in result.error
