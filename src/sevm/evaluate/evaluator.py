"""The evaluator itself: compile, swap in, run, decode, revert.

Borrow the real Solidity compiler instead of writing an interpreter. Splice

    function __sevm_eval() public payable returns (T) { return (<expr>); }

into the paused contract's source, compile it, swap the runtime code at the target address
on a state snapshot, run it, decode the output, revert. Operator precedence, checked
arithmetic, `ether`/`days` units, casts, `keccak256`, `abi.encode`, struct and mapping
access, internal calls and every future Solidity feature come out correct for free.
"""

from __future__ import annotations

import re
import time
from collections.abc import Sequence
from typing import Any

from eth_abi import decode as abi_decode
from eth_utils import function_signature_to_4byte_selector

from ..compile import Artifact, CompileError, Project, compile_standard
from ..decode import decode_revert
from .bindings import (
    MSG_FIELDS,
    Binding,
    _call_data,
    _parameter_list,
    bindings_for,
    msg_bindings,
    rewrite_msg,
    unbindable_reference,
)
from .injection import (
    _TYPE_ERROR,
    _VOID_ERROR,
    EVAL_FUNCTION,
    PROBE_TYPE,
    EvalError,
    EvalResult,
    _abi_type_of,
    _format_value,
    _inject,
    _normalise_type,
)

EVAL_SELECTOR = function_signature_to_4byte_selector(f"{EVAL_FUNCTION}()")

_IDENTIFIER = re.compile(r"^[A-Za-z_$][A-Za-z0-9_$]*$")


class Evaluator:
    """Compiles and runs Solidity expressions against a paused VM."""

    def __init__(self, project: Project) -> None:
        self.project = project
        self._code_cache: dict[tuple[str, str, str], tuple[bytes, str]] = {}
        self._type_cache: dict[tuple[str, str, str], str | None] = {}
        self._closure_cache: dict[str, frozenset[str] | None] = {}
        self._contracts_by_id: dict[int, dict] | None = None
        self.compile_count = 0

    # -- compilation --------------------------------------------------------

    def _closure(self, source_key: str) -> frozenset[str] | None:
        """The source keys reachable from `source_key` by imports.

        Solidity resolves names per source unit, so a source outside the closure could
        not have been named by the expression anyway: sending it to solc buys nothing
        and costs a parse. On a forge-std project that is most of the compile. None when
        the import graph cannot be read off the ASTs, which sends everything instead.
        """
        if source_key in self._closure_cache:
            return self._closure_cache[source_key]
        seen: set[str] = set()
        queue = [source_key]
        while queue:
            key = queue.pop()
            if key in seen:
                continue
            ast = self.project.asts.get(key)
            if ast is None or key not in self.project.sources:
                self._closure_cache[source_key] = None
                return None
            seen.add(key)
            queue.extend(
                node["absolutePath"]
                for node in ast.get("nodes", [])
                if node.get("nodeType") == "ImportDirective" and node.get("absolutePath")
            )
        self._closure_cache[source_key] = frozenset(seen)
        return self._closure_cache[source_key]

    def _sources_with(
        self,
        artifact: Artifact,
        expression: str,
        return_type: str | None,
        probe: bool = False,
        parameters: str = "",
    ) -> dict[str, str]:
        keys = self._closure(artifact.source_key)
        sources = {
            key: src.text
            for key, src in self.project.sources.items()
            if keys is None or key in keys
        }
        if artifact.source_key not in sources:
            raise EvalError(f"no source available for {artifact.qualified_name}")
        sources[artifact.source_key] = _inject(
            sources[artifact.source_key],
            expression,
            return_type,
            artifact.source_range,
            probe=probe,
            parameters=parameters,
        )
        return sources

    def _try_compile(
        self,
        artifact: Artifact,
        expression: str,
        return_type: str | None,
        probe: bool = False,
        parameters: str = "",
    ) -> dict:
        self.compile_count += 1
        return compile_standard(
            self._sources_with(
                artifact, expression, return_type, probe=probe, parameters=parameters
            ),
            solc_version=self.project.solc_version,
            optimize=False,
            # Only the one contract is ever read back, and codegen for the rest of a
            # library is the other half of the compile.
            output_selection={
                artifact.source_key: {artifact.name: ["evm.deployedBytecode.object"]}
            },
            remappings=self.project.remappings or None,
        )

    def _contract_nodes(self) -> dict[int, dict]:
        """Every ContractDefinition in the project, by AST id."""
        if self._contracts_by_id is None:
            self._contracts_by_id = {
                node["id"]: node
                for ast in self.project.asts.values()
                for node in ast.get("nodes", [])
                if node.get("nodeType") == "ContractDefinition" and "id" in node
            }
        return self._contracts_by_id

    def _known_type(
        self, artifact: Artifact, expression: str, bindings: Sequence[Binding] = ()
    ) -> str | None:
        """The type of a bare identifier, without paying for the probe compile.

        `p amount` and `p totalDeposits` are most of what gets typed at a prompt, and
        typing them costs a whole compile of its own. This is a lookup rather than a
        second type checker: a bound local resolves to the parameter sevm itself
        declares, and a state variable to solc's own `typeString`, so both are the
        string the probe's diagnostic would have reported. Anything else returns None
        and goes to solc.
        """
        if not _IDENTIFIER.match(expression):
            return None
        for binding in bindings:
            if binding.name == expression:
                return _normalise_type(binding.declared_type)
        nodes = self._contract_nodes()
        own = next(
            (
                node
                for node in (self.project.asts.get(artifact.source_key) or {}).get(
                    "nodes", []
                )
                if node.get("nodeType") == "ContractDefinition"
                and node.get("name") == artifact.name
            ),
            None,
        )
        if own is None:
            return None
        # Linearized bases are C3 order, most derived first, so the first declaration
        # found is the one that shadows, exactly as solc resolves it.
        for contract_id in own.get("linearizedBaseContracts") or [own.get("id")]:
            base = nodes.get(contract_id)
            for node in (base or {}).get("nodes", []):
                if (
                    node.get("nodeType") == "VariableDeclaration"
                    and node.get("stateVariable")
                    and node.get("name") == expression
                ):
                    declared = (node.get("typeDescriptions") or {}).get("typeString")
                    return _normalise_type(declared) if declared else None
        return None

    def _infer_type(
        self,
        artifact: Artifact,
        expression: str,
        parameters: str = "",
        bindings: Sequence[Binding] = (),
    ) -> str | None:
        """The expression's type, from the AST if it is a bare name, else from solc."""
        key = (artifact.qualified_name, expression, parameters)
        if key in self._type_cache:
            return self._type_cache[key]
        known = self._known_type(artifact, expression, bindings)
        if known is not None:
            self._type_cache[key] = known
            return known
        return self._probe_type(artifact, expression, parameters)

    def _probe_type(
        self, artifact: Artifact, expression: str, parameters: str = ""
    ) -> str | None:
        """Ask solc the expression's type by making it complain.

        Compiles the expression as a return of an unreachable struct type (nothing
        converts to it), so the diagnostic always names the true type. Returns None for
        a void expression (a bare call), compiled as a statement instead.
        """
        key = (artifact.qualified_name, expression, parameters)
        try:
            self._try_compile(
                artifact, expression, PROBE_TYPE, probe=True, parameters=parameters
            )
        except CompileError as exc:
            text = str(exc)
            match = _TYPE_ERROR.search(text)
            if match:
                result = _normalise_type(match.group(1))
            elif _VOID_ERROR.search(text):
                result = None
            else:
                raise EvalError(_clean_solc_error(text, expression)) from exc
        else:
            # The probe compiled, which means the expression really is that struct.
            raise EvalError("expression has an internal type that cannot be displayed")
        self._type_cache[key] = result
        return result

    def _compiled(
        self,
        artifact: Artifact,
        expression: str,
        parameters: str = "",
        bindings: Sequence[Binding] = (),
    ) -> tuple[bytes, str | None]:
        # The parameter list is part of the key, not the values it carries: the same
        # expression over the same locals compiles once and is reused at every stop.
        key = (artifact.qualified_name, expression, parameters)
        cached = self._code_cache.get(key)
        if cached is not None:
            code, declared = cached
            return code, (declared or None)

        known = self._known_type(artifact, expression, bindings)
        return_type = self._infer_type(artifact, expression, parameters, bindings)
        try:
            out = self._try_compile(
                artifact, expression, return_type, parameters=parameters
            )
        except CompileError as exc:
            if known is None:
                raise EvalError(_clean_solc_error(str(exc), expression)) from exc
            # A type read off the AST is the only thing here that solc did not say
            # itself, so a failure to compile means the probe gets the last word.
            self._type_cache.pop(key, None)
            return_type = self._probe_type(artifact, expression, parameters)
            try:
                out = self._try_compile(
                    artifact, expression, return_type, parameters=parameters
                )
            except CompileError as retry:
                raise EvalError(_clean_solc_error(str(retry), expression)) from retry

        contracts = out.get("contracts", {}).get(artifact.source_key, {})
        data = contracts.get(artifact.name)
        if not data:
            raise EvalError(f"compiled output missing {artifact.name}")
        code = bytes.fromhex(data["evm"]["deployedBytecode"]["object"])
        self._code_cache[key] = (code, return_type or "")
        return code, return_type

    # -- execution ----------------------------------------------------------

    def evaluate(
        self,
        session: Any,
        frame: Any,
        computation: Any,
        expression: str,
        keep: bool = False,
        want_bool: bool = False,
        bindings: Sequence[Any] = (),
    ) -> Any:
        """Evaluate `expression` in the paused frame.

        Runs on the VM thread while the hook is parked. Effects revert unless `keep` is
        set (gdb's `call` verb: evaluate and commit).
        """
        expression = expression.strip().rstrip(";")
        if not expression:
            raise EvalError("empty expression")

        artifact: Artifact | None = getattr(frame, "artifact", None)
        if artifact is None:
            raise EvalError(
                "no Solidity source for the contract in this frame; "
                "use the low-level views ($stack, $storage, x/) instead"
            )

        blocked = unbindable_reference(expression, bindings)
        if blocked:
            raise EvalError(blocked)
        # `expression` stays as typed, for the result and the history; `compiled` is the
        # same expression with msg.data/msg.sig turned into parameters.
        compiled, msg_fields = rewrite_msg(expression)
        bound = bindings_for(compiled, bindings) + msg_bindings(frame, msg_fields)
        parameters = _parameter_list(bound)

        started = time.time()
        code, return_type = self._compiled(artifact, compiled, parameters, bound)
        compile_ms = (time.time() - started) * 1000.0

        state = computation.state
        address = frame.address
        original_code = state.get_code(address)
        snapshot = state.snapshot()
        gas_before = 0
        try:
            state.set_code(address, code)
            message = _build_message(frame, computation, code, _call_data(bound))
            txctx = computation.transaction_context
            with session.suspended():
                sub = state.computation_class.apply_message(state, message, txctx)
            gas_before = message.gas - sub.get_gas_remaining()
            if sub.is_error:
                reason = decode_revert(bytes(sub.output or b""), artifact.abi)
                raise EvalError(f"expression {reason}")
            raw = bytes(sub.output)
        finally:
            if keep:
                state.set_code(address, original_code)
                state.commit(snapshot)
            else:
                state.revert(snapshot)

        if return_type is None:
            result = EvalResult(
                expression=expression,
                type_name="void",
                abi_type="",
                value=None,
                display="<void>",
                raw=raw,
                gas_used=gas_before,
                compile_ms=compile_ms,
                kept=keep,
                void=True,
            )
            return True if want_bool else result

        abi_type = _abi_type_of(return_type)
        try:
            value = abi_decode([abi_type], raw)[0]
        except Exception as exc:
            raise EvalError(
                f"could not decode {abi_type} from 0x{raw.hex()}: {exc}"
            ) from exc

        if abi_type == "address" and isinstance(value, str):
            value = value.lower()

        if want_bool:
            return bool(value)

        return EvalResult(
            expression=expression,
            type_name=return_type,
            abi_type=abi_type,
            value=value,
            display=_format_value(value, abi_type),
            raw=raw,
            gas_used=gas_before,
            compile_ms=compile_ms,
            kept=keep,
        )

    def type_of(
        self,
        session: Any,
        frame: Any,
        computation: Any,
        expression: str,
        bindings: Sequence[Any] = (),
    ) -> str:
        """Backs `ptype`. Falls straight out of the inference we already do."""
        artifact: Artifact | None = getattr(frame, "artifact", None)
        if artifact is None:
            raise EvalError("no Solidity source for the contract in this frame")
        expression = expression.strip().rstrip(";")
        blocked = unbindable_reference(expression, bindings)
        if blocked:
            raise EvalError(blocked)
        compiled, msg_fields = rewrite_msg(expression)
        bound = bindings_for(compiled, bindings) + msg_bindings(frame, msg_fields)
        return (
            self._infer_type(artifact, compiled, _parameter_list(bound), bound) or "void"
        )


def _build_message(
    frame: Any, computation: Any, code: bytes, data: bytes = EVAL_SELECTOR
) -> Any:
    """A message that mirrors the paused frame so `msg.*` reads truthfully.

    `should_transfer_value` is off so `msg.value` reports the paused frame's value
    without moving ether a second time.
    """
    from eth.vm.message import Message

    return Message(
        gas=max(computation.get_gas_remaining(), 1_000_000),
        to=frame.address,
        sender=frame.sender,
        value=frame.value,
        data=data,
        code=code,
        depth=min(frame.depth + 1, 1023),
        should_transfer_value=False,
        is_static=False,
    )


def _clean_solc_error(text: str, expression: str) -> str:
    """Trim solc's standard-json wall of text down to the message the user needs."""
    lines = [ln.strip() for ln in text.splitlines() if ln.strip()]
    for line in lines:
        if line.startswith(
            ("TypeError:", "DeclarationError:", "ParserError:", "SyntaxError:")
        ):
            message = line.split(":", 1)[1].strip()
            return _undo_msg_rewrite(f"{message} (in `{expression}`)")
    return _undo_msg_rewrite(f"could not compile `{expression}`")


def _undo_msg_rewrite(text: str) -> str:
    """Report the error against `msg.data`, not the parameter it was rewritten to."""
    for field, (name, _declared, _abi) in MSG_FIELDS.items():
        text = text.replace(name, f"msg.{field}")
    return text


def make_eval_hook(evaluator: Evaluator):
    """Adapter matching DebugSession.set_eval_hook's signature."""

    def hook(
        session, frame, computation, expression, keep=False, want_bool=False, bindings=()
    ):
        return evaluator.evaluate(
            session,
            frame,
            computation,
            expression,
            keep=keep,
            want_bool=want_bool,
            bindings=bindings,
        )

    return hook
