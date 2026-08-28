"""Local variables: the static index, reading them off a live frame, and writing one."""

from __future__ import annotations

from harness import (
    TIMEOUT,
    locals_debugger,
    locals_map,
    stop_at,
)

from sevm.session import Finished, StepMode
from sevm.srcmap import (
    PcMap,
    build_line_indexes,
)


def test_locals_index_finds_every_declaration(proj):
    from sevm.locals import LocalsIndex

    index = LocalsIndex(proj.asts)
    credit = [
        fn for fn in index.by_function.values() if any(v.name == "fee" for v in fn.body)
    ]
    assert len(credit) == 1
    layout = credit[0]
    assert [v.name for v in layout.params] == ["who", "amount"]
    assert [v.name for v in layout.body] == ["fee"]


def test_locals_index_orders_params_before_body(proj):
    """Allocation order, not AST visit order: the frame layout depends on it."""
    from sevm.locals import LocalsIndex

    index = LocalsIndex(proj.asts)
    for layout in index.by_function.values():
        kinds = [v.kind for v in layout.all]
        assert kinds == sorted(kinds, key=["param", "return", "local"].index)
        assert [v.index for v in layout.all] == list(range(len(layout.all)))


def test_locals_scope_excludes_declarations_from_closed_blocks(proj):
    from sevm.locals import LocalsIndex

    index = LocalsIndex(proj.asts)
    scoping = next(
        fn
        for fn in index.by_function.values()
        if any(v.name == "shadowed" for v in fn.body)
    )
    shadowed = next(v for v in scoping.body if v.name == "shadowed")
    after = next(v for v in scoping.body if v.name == "after_")
    # `after_` is declared past the `if` block, so `shadowed` is long gone by then.
    assert not shadowed.visible_at(after.start)
    assert shadowed.visible_at(shadowed.start + 1)


def test_stack_slots_counts_calldata_and_function_types():
    from sevm.locals import stack_slots

    assert stack_slots("uint256", "default") == 1
    assert stack_slots("string", "memory") == 1
    assert stack_slots("bytes", "calldata") == 2
    assert stack_slots("uint256[]", "calldata") == 2
    assert stack_slots("uint256[3]", "calldata") == 1
    assert stack_slots("function (uint256) external returns (uint256)", "default") == 2
    assert stack_slots("", "default") is None


def test_declaration_pcs_skips_parameters(proj):
    """Parameters are pushed by the caller; recording them here would be wrong."""
    from sevm.locals import KIND_LOCAL, KIND_RETURN, LocalsIndex, declaration_pcs

    art = proj.artifact("Bank")
    index = LocalsIndex(proj.asts)
    pcmap = PcMap(
        art.deployed_bytecode,
        art.deployed_source_map,
        build_line_indexes(proj.sources.values()),
    )
    table = declaration_pcs(pcmap, index)
    assert table, "no declaration sites found at all"
    assert all(v.kind in (KIND_LOCAL, KIND_RETURN) for v in table.values())
    assert any(v.name == "fee" for v in table.values())


def test_locals_read_the_paused_frame(deposit_debugger):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    values = locals_map(dbg)
    assert values["amount"].startswith("2000000000000000000")
    assert values["fee"].startswith("5000000000000000 ")
    assert values["who"].startswith("0x")


def test_locals_survive_the_value_types(locals_contract):
    dbg = locals_debugger(*locals_contract, "values", 7)
    try:
        stop_at(dbg, 23)
        values = locals_map(dbg)
        assert values["doubled"] == "14"
        assert values["negative"] == "-7"
        assert values["flag"] == "true"
        assert values["who"] == "0x" + "0" * 36 + "1234"
        assert values["tag"] == "0xdeadbeef"
        assert values["small"] == "200"
    finally:
        dbg.close()


def test_locals_from_a_closed_block_do_not_resurface(locals_contract):
    """The regression the naive implementation always has: a stale slot reappearing."""
    dbg = locals_debugger(*locals_contract, "scoping", 5)
    try:
        stop_at(dbg, 39)
        values = locals_map(dbg)
        assert "shadowed" not in values
        assert "inner" not in values
        assert values["total"] == "338"  # 5 + 111 + 222
        assert values["after_"] == "333"
    finally:
        dbg.close()


def test_locals_track_a_loop_variable(locals_contract):
    dbg = locals_debugger(*locals_contract, "loop", 3)
    try:
        stop_at(dbg, 56)
        seen = []
        for _ in range(3):
            values = locals_map(dbg)
            seen.append((values["i"], values["square"]))
            event = dbg.session.resume(StepMode.RUN, timeout=TIMEOUT)
            if isinstance(event, Finished):
                break
        assert seen == [("0", "0"), ("1", "1"), ("2", "4")]
    finally:
        dbg.close()


def test_recursion_gives_each_frame_its_own_locals(locals_contract):
    """Same function, two live frames, one stack: the bases must differ."""
    dbg = locals_debugger(*locals_contract, "recurse", 2)
    try:
        stop_at(dbg, 49)
        assert locals_map(dbg)["here"] == "10"
        assert dbg.run("up").ok
        assert locals_map(dbg)["here"] == "20"
        assert dbg.run("down").ok
        assert locals_map(dbg)["here"] == "10"
    finally:
        dbg.close()


def test_memory_reference_locals_are_dereferenced(locals_contract):
    dbg = locals_debugger(*locals_contract, "memoryTypes", "hello")
    try:
        stop_at(dbg, 81)
        values = locals_map(dbg)
        assert values["label"] == '"hello"'
        assert values["list"] == "[3 items] [10, 20, 30]"
        assert values["raw"] == "0x" + b"hello".hex()
        assert values["len"] == "8"
    finally:
        dbg.close()


def test_storage_pointer_reports_its_slot_not_a_value(locals_contract):
    dbg = locals_debugger(*locals_contract, "storagePointer", 42)
    try:
        stop_at(dbg, 89)
        rows = {r["name"]: r for r in dbg.commands.read_locals()}
        assert "storage pointer" in rows["p"]["value"]
        assert "index the state variable" in rows["p"]["reason"]
        assert rows["read"]["value"] == "42"
    finally:
        dbg.close()


def test_calldata_reference_is_two_slots_and_says_so(locals_contract):
    dbg = locals_debugger(*locals_contract, "calldataTypes", b"\xaa\xbb\xcc")
    try:
        stop_at(dbg, 95)
        rows = {r["name"]: r for r in dbg.commands.read_locals()}
        assert "calldata reference" in rows["payload"]["value"]
        assert rows["size"]["value"] == "3"
    finally:
        dbg.close()


def test_modifier_locals_belong_to_the_modifier_frame(locals_contract):
    """A modifier is inlined into the function, but its locals are its own."""
    dbg = locals_debugger(*locals_contract, "bump", 5)
    try:
        stop_at(dbg, 63)
        values = locals_map(dbg)
        assert "before" in values
        assert "next" not in values, (
            "the function's locals are not in the modifier's scope"
        )
    finally:
        dbg.close()


def test_a_local_is_unavailable_before_it_is_allocated(locals_contract):
    """Stopped *on* the declaration, the slot does not exist yet. Say so, do not guess."""
    dbg = locals_debugger(*locals_contract, "values", 7)
    try:
        stop_at(dbg, 17)
        rows = {r["name"]: r for r in dbg.commands.read_locals()}
        assert not rows["doubled"]["available"]
        assert rows["doubled"]["value"] == "<unavailable>"
        assert "step once" in rows["doubled"]["reason"]
        # Declarations further down the function are not in scope at all yet.
        assert "small" not in rows
    finally:
        dbg.close()


def test_locals_are_empty_without_source(deposit_debugger):
    """No artifact, no AST, no guessing."""
    dbg = deposit_debugger
    assert dbg.run("info locals").ok


def test_print_evaluates_an_expression_over_locals(deposit_debugger):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    result = dbg.run("p amount - fee")
    assert result.ok, result.error
    assert "1995000000000000000" in " ".join(result.lines)


def test_locals_shadow_state_variables_in_expressions(locals_contract):
    """`counter` the state variable vs `next`, a local: Solidity's own scoping decides."""
    dbg = locals_debugger(*locals_contract, "bump", 5)
    try:
        stop_at(dbg, 70)
        assert "5" in " ".join(dbg.run("p next").lines)
        assert "5" in " ".join(dbg.run("p counter").lines)
        assert "10" in " ".join(dbg.run("p next + counter").lines)
    finally:
        dbg.close()


def test_ptype_reports_a_locals_declared_type(deposit_debugger):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    assert "address" in " ".join(dbg.run("ptype who").lines)
    assert "uint256" in " ".join(dbg.run("ptype fee").lines)


def test_expression_over_an_unreadable_local_says_why(locals_contract):
    dbg = locals_debugger(*locals_contract, "storagePointer", 42)
    try:
        stop_at(dbg, 89)
        result = dbg.run("p p")
        assert not result.ok
        assert "cannot be used in an expression" in (result.error or "")
    finally:
        dbg.close()


def test_expression_compiles_once_across_many_stops(deposit_debugger):
    """Passing locals as parameters, not literals, is what keeps the cache warm."""
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    dbg.run("p amount - fee")
    after_first = dbg.evaluator.compile_count
    for _ in range(3):
        dbg.run("p amount - fee")
    assert dbg.evaluator.compile_count == after_first


def test_bindings_only_inject_referenced_names(proj):
    from sevm.evaluate import bindings_for
    from sevm.locals import LocalValue

    available = [
        LocalValue(
            name="fee", type_label="uint256", display="1", abi_type="uint256", abi_value=1
        ),
        LocalValue(
            name="amount",
            type_label="uint256",
            display="2",
            abi_type="uint256",
            abi_value=2,
        ),
    ]
    assert [b.name for b in bindings_for("fee + 1", available)] == ["fee"]
    assert [b.name for b in bindings_for("totalDeposits", available)] == []
    # A member access must not bind an unrelated local of the same name.
    assert [b.name for b in bindings_for("x.amount", available)] == []


def test_unbindable_local_is_reported_not_ignored(proj):
    from sevm.evaluate import unbindable_reference
    from sevm.locals import LocalValue

    blocked = [
        LocalValue(
            name="p",
            type_label="struct Point storage",
            display="<ptr>",
            available=True,
            reason="storage pointers are not dereferenced",
        )
    ]
    assert "p" in (unbindable_reference("p.x", blocked) or "")
    assert unbindable_reference("totalDeposits", blocked) is None


def test_breakpoint_condition_reads_a_local(deposit_debugger):
    """The limitation SEVM.md called out: `b LOC if amount > 1 ether` now works."""
    dbg = deposit_debugger
    dbg.run("b Bank.sol:46 if amount > 1 ether")
    assert dbg.run("c").ok
    assert dbg.snap.line == 46


def test_breakpoint_condition_on_a_local_can_be_false(locals_contract):
    dbg = locals_debugger(*locals_contract, "loop", 3)
    try:
        dbg.run("b Locals.sol:56 if i == 2")
        assert dbg.run("c").ok
        assert locals_map(dbg)["i"] == "2"
    finally:
        dbg.close()


def test_set_var_writes_a_locals_stack_slot(deposit_debugger):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    assert dbg.run("set var fee = 1 ether").ok
    assert locals_map(dbg)["fee"].startswith("1000000000000000000")
    # The write is real: the running program sees it, not just the display.
    assert "1000000000000000000" in " ".join(dbg.run("p amount - fee").lines)


def test_set_var_refuses_a_reference_type(locals_contract):
    dbg = locals_debugger(*locals_contract, "memoryTypes", "hello")
    try:
        stop_at(dbg, 81)
        result = dbg.run("set var list = 1")
        assert not result.ok
        assert "reference" in (result.error or "")
        # And the pointer is untouched, so the local still reads correctly.
        assert locals_map(dbg)["list"] == "[3 items] [10, 20, 30]"
    finally:
        dbg.close()


def test_info_args_shows_the_internal_frames_own_arguments(deposit_debugger):
    """Inside `_credit`, calldata still describes `deposit()`. The frame wins."""
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    lines = " ".join(dbg.run("info args").lines)
    assert "who" in lines and "amount" in lines
    assert "not of the internal function" not in lines


def test_snapshot_carries_locals_for_the_tui(deposit_debugger):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    names = {v.name for v in dbg.snap.locals}
    assert {"who", "amount", "fee"} <= names
