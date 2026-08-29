"""The stop policy: step, next, finish, and the frames they move between."""

from __future__ import annotations

from harness import (
    Debugger,
    line_of,
    locals_debugger,
)

from sevm.session import Finished, Paused, StepMode


def test_session_opens_on_the_called_function(deposit_debugger):
    snap = deposit_debugger.snap
    assert isinstance(deposit_debugger.first, Paused)
    assert snap.function.display_name == "Bank.deposit"
    assert snap.contract_name == "Bank"
    assert snap.depth == 0


def test_step_enters_internal_functions(deposit_debugger):
    dbg = deposit_debugger
    seen = []
    for _ in range(6):
        event = dbg.step(StepMode.STEP)
        if isinstance(event, Finished):
            break
        seen.append(event.snapshot.function.name)
    assert "_credit" in seen, "step must enter internal calls"
    assert "_fee" in seen, "step must enter nested internal calls"


def test_next_steps_over_internal_functions(deposit_debugger):
    dbg = deposit_debugger
    seen = []
    for _ in range(6):
        event = dbg.step(StepMode.NEXT)
        if isinstance(event, Finished):
            break
        seen.append(event.snapshot.function.name)
    assert "_fee" not in seen, "next must not descend into _fee"
    assert "deposit" in seen or "_credit" in seen


def test_next_enters_the_body_of_the_function_it_stopped_on(deposit_debugger):
    """solc marks the dispatcher's jump into a body as an internal call; `next` at the
    function's opening line must still reach the first statement."""
    dbg = deposit_debugger
    start_line = dbg.snap.line
    event = dbg.step(StepMode.NEXT)
    assert isinstance(event, Paused)
    assert event.snapshot.line > start_line
    assert event.snapshot.function.display_name == "Bank.deposit"


def test_stepi_and_nexti_move_one_opcode(deposit_debugger):
    dbg = deposit_debugger
    before = dbg.snap.step
    dbg.step(StepMode.STEPI)
    assert dbg.snap.step == before + 1
    dbg.step(StepMode.NEXTI)
    assert dbg.snap.step == before + 2


def test_step_count_repeats(deposit_debugger):
    dbg = deposit_debugger
    before = dbg.snap.step
    dbg.step(StepMode.STEPI, count=5)
    assert dbg.snap.step == before + 5


def test_backtrace_interleaves_solidity_and_evm_frames(deposit_debugger):
    dbg = deposit_debugger
    for _ in range(4):
        if isinstance(dbg.step(StepMode.STEP), Finished):
            break
    rows = dbg.snap.backtrace
    assert rows[-1].kind == "evm"
    names = [r.name for r in rows]
    assert any("_fee" in n or "_credit" in n for n in names)
    # Outer frames show their call site, not their entry point.
    assert all(r.line >= 0 for r in rows)
    # No compiler-generated frame is shown unless we are stopped inside one.
    assert not any(r.detail == "compiler-generated" for r in rows[1:])


def test_cross_contract_call_creates_an_evm_frame(bank):
    w3, proj_, contract, callee, _alice = bank

    def txfn():
        tx = contract.functions.forward(callee.address, 21).transact({"gas": 400_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    try:
        dbg.session.break_at_function("receiveValue")
        event = dbg.step(StepMode.RUN)
        assert isinstance(event, Paused)
        snap = event.snapshot
        assert snap.depth == 1
        assert snap.contract_name == "Callee"
        kinds = [r.kind for r in snap.backtrace]
        assert kinds.count("evm") == 2, "both EVM frames must appear"
        assert any("Bank.forward" in r.name for r in snap.backtrace)
    finally:
        dbg.close()


def test_finish_leaves_the_current_frame(bank):
    w3, proj_, contract, callee, _alice = bank

    def txfn():
        tx = contract.functions.forward(callee.address, 21).transact({"gas": 400_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    try:
        dbg.session.break_at_function("receiveValue")
        dbg.step(StepMode.RUN)
        assert dbg.snap.depth == 1
        event = dbg.step(StepMode.FINISH)
        assert isinstance(event, Paused)
        assert event.snapshot.stop_reason == "finish"
    finally:
        dbg.close()


def test_loop_iterates_with_next(bank):
    w3, proj_, contract, _callee, alice = bank
    tx = contract.functions.deposit().transact(
        {"from": alice.address, "value": w3.to_wei(1, "ether"), "gas": 300_000}
    )
    w3.eth.wait_for_transaction_receipt(tx)

    def txfn():
        contract.functions.sumHistory().call()

    dbg = Debugger(proj_, txfn)
    try:
        body = line_of(proj_, "total += history[i];")
        hits = 0
        for _ in range(24):
            event = dbg.step(StepMode.NEXT)
            if isinstance(event, Finished):
                break
            if event.snapshot.line == body:
                hits += 1
        assert hits >= 1, "the loop body should be reached"
    finally:
        dbg.close()


# --- reseat / bind: naming a frame reached outside solc's calling convention -------
#
# A JOP gadget corrupts a JUMP's destination to land inside a function without the
# `i`-tagged jump solc emits for a real call, so the internal-frame model stays pinned
# to the function it last legitimately entered. `reseat` re-names the frame at the pc;
# `bind` pins a body local the landing skipped the prologue for.


def _cross_function_jumpdest(session, frame, current_ast_id):
    """A JUMPDEST inside a *different* Solidity function than the one we are in."""
    for pc in sorted(frame.disassembly.jumpdests):
        loc = frame.location(pc)
        fn = session.functions.at_location(loc)
        if fn is not None and fn.ast_id != current_ast_id:
            return pc, fn
    return None, None


def _body_jumpdest_where_visible(session, frame, fn, name):
    """A JUMPDEST in `fn` where the local `name` is in scope."""
    for pc in sorted(frame.disassembly.jumpdests):
        loc = frame.location(pc)
        if loc is None or loc.is_generated:
            continue
        here = session.functions.at_location(loc)
        if here is None or here.ast_id != fn.ast_id:
            continue
        if any(
            v.name == name for v in session.locals.visible(fn.ast_id, loc.entry.start)
        ):
            return pc
    return None


def test_reseat_names_the_function_reached_by_a_hand_jump(locals_contract):
    w3, proj_, contract = locals_contract
    dbg = locals_debugger(w3, proj_, contract, "values", 7)
    try:
        dbg.run("b Locals.sol:22")
        assert dbg.run("c").ok
        assert dbg.snap.backtrace[0].name.startswith("Locals.values")

        frame = dbg.session._frames[-1]
        target_pc, target_fn = _cross_function_jumpdest(
            dbg.session, frame, dbg.snap.function.ast_id
        )
        assert target_pc is not None, "no cross-function JUMPDEST to test with"

        # Land there for real: `jump` sets the pc, `stepi` executes the JUMPDEST so the
        # pause (and its snapshot) is taken at the target, as a gadget's jump would be.
        dbg.run(f"jump 0x{target_pc:x}")
        dbg.step(StepMode.STEPI)
        # The pc-derived header moved, but the frame model is still pinned to `values`.
        assert dbg.snap.backtrace[0].name.startswith("Locals.values")

        result = dbg.run("reseat")
        assert result.ok, result.error
        assert dbg.snap.backtrace[0].name.startswith(f"Locals.{target_fn.name}")
    finally:
        dbg.close()


def test_bind_recovers_a_local_a_jump_landing_could_not_observe(locals_contract):
    w3, proj_, contract = locals_contract
    dbg = locals_debugger(w3, proj_, contract, "scoping", 5)
    try:
        dbg.run("b Locals.sol:31")
        assert dbg.run("c").ok

        scoping = dbg.session.functions.find("scoping")[0]
        frame = dbg.session._frames[-1]
        pc = _body_jumpdest_where_visible(dbg.session, frame, scoping, "total")
        assert pc is not None, "no JUMPDEST with `total` in scope"

        dbg.run(f"jump 0x{pc:x}")
        dbg.step(StepMode.STEPI)
        dbg.run(
            "reseat"
        )  # a fresh frame: its slots are cleared, so `total` is unobserved

        before = {r["name"]: r for r in dbg.commands.read_locals()}
        assert not before["total"]["available"], "a jump landing cannot observe it"

        # Point the local at a slot we control, the way you would at a crafted stack.
        dbg.run("set $stack[0] = 0x2a")
        result = dbg.run("bind total = $stack[0]")
        assert result.ok, result.error

        after = {r["name"]: r for r in dbg.commands.read_locals()}
        assert after["total"]["available"]
        assert after["total"]["value"] == "42"
    finally:
        dbg.close()


def test_bind_rejects_a_name_not_in_scope(locals_contract):
    w3, proj_, contract = locals_contract
    dbg = locals_debugger(w3, proj_, contract, "values", 7)
    try:
        dbg.run("b Locals.sol:22")
        assert dbg.run("c").ok
        result = dbg.run("bind not_a_local = $stack[0]")
        assert not result.ok
        assert "not_a_local" in result.error
    finally:
        dbg.close()
