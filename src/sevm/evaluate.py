"""Solidity expression evaluation at a breakpoint.

The trick, verified in research/spikes/spike_solidity_eval.py: instead of writing a
Solidity interpreter, borrow the real one. Splice

    function __sevm_eval() public payable returns (T) { return (<expr>); }

into the paused contract's own source, compile it, swap the runtime code at the target
address on a state snapshot, run it, decode the output, and revert.

What that buys, for free and permanently correct: operator precedence, checked
arithmetic, `ether`/`gwei`/`days` units, casts, `keccak256`, `abi.encode`, struct and
mapping access, calls to internal and private functions, and every future Solidity
feature. A hand-written evaluator would approximate all of it and get some of it wrong.

The return type is not known up front, so the first attempt compiles as `uint256` and, on
a mismatch, the real type is read out of solc's own diagnostic. Two compiles worst case,
about 40 ms, and the compiled result is cached per (source, expression).

Local variables ride in as *parameters* of the injected function, with their values in
the call's calldata:

    function __sevm_eval(uint256 fee, uint256 amount) public payable returns (uint256)
    { return (amount - fee); }

Passing them rather than splicing them in as literals is what keeps the cache useful. The
compiled code depends only on the names and types, which do not change while you sit on a
line, so `display amount - fee` costs one compile for the whole session instead of one per
step. Solidity's own scoping does the rest: a parameter shadows a state variable of the
same name, exactly as the real local does.
"""

from __future__ import annotations

import re
import time
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any

from eth_abi import decode as abi_decode
from eth_abi import encode as abi_encode
from eth_utils import function_signature_to_4byte_selector

from .compile import Artifact, CompileError, Project, compile_standard
from .decode import decode_revert
from .locals import referenced_names

EVAL_FUNCTION = "__sevm_eval"
EVAL_SELECTOR = function_signature_to_4byte_selector(f"{EVAL_FUNCTION}()")


@dataclass(frozen=True)
class Binding:
    """One local variable, ready to be passed into the injected eval function."""

    name: str
    declared_type: str  # as written in the parameter list, e.g. "string memory"
    abi_type: str  # as abi-encoded, e.g. "string"
    value: Any

    @property
    def parameter(self) -> str:
        return f"{self.declared_type} {self.name}"


def _declared_parameter_type(abi_type: str) -> str:
    """A parameter list needs a data location on every reference type."""
    if (
        abi_type in ("string", "bytes")
        or abi_type.endswith("]")
        or abi_type.startswith("(")
    ):
        return f"{abi_type} memory"
    return abi_type


def bindings_for(expression: str, locals_available: Sequence[Any]) -> list[Binding]:
    """Pick the locals an expression actually mentions.

    Only what is referenced gets injected. A frame full of locals we cannot materialise
    must not stop `p totalDeposits` from working, and an unreferenced name has no
    business changing the compiled code or the cache key.
    """
    wanted = referenced_names(expression)
    out: list[Binding] = []
    seen = set()
    for local in locals_available:
        if local.name not in wanted or local.name in seen:
            continue
        if not getattr(local, "bindable", False):
            continue
        seen.add(local.name)
        out.append(
            Binding(
                name=local.name,
                declared_type=_declared_parameter_type(local.abi_type),
                abi_type=local.abi_type,
                value=local.abi_value,
            )
        )
    return out


def unbindable_reference(expression: str, locals_available: Sequence[Any]) -> str | None:
    """The error for an expression that names a local we cannot pass in.

    Without this the name would quietly resolve to a state variable of the same name, or
    to nothing, and the user would be told the identifier is undeclared when it is right
    there on the screen.
    """
    wanted = referenced_names(expression)
    for local in locals_available:
        if local.name in wanted and not getattr(local, "bindable", False):
            detail = local.reason or "not readable here"
            return f"local `{local.name}` cannot be used in an expression: {detail}"
    return None


def _call_data(bindings: Sequence[Binding]) -> bytes:
    """Selector plus arguments for the injected function, as a real ABI call."""
    types = [b.abi_type for b in bindings]
    signature = f"{EVAL_FUNCTION}({','.join(types)})"
    selector = function_signature_to_4byte_selector(signature)
    if not bindings:
        return selector
    return selector + abi_encode(types, [b.value for b in bindings])


def _parameter_list(bindings: Sequence[Binding]) -> str:
    return ", ".join(b.parameter for b in bindings)


# Type probe. Nothing in Solidity implicitly converts to a locally-declared struct, so
# declaring this as the return type guarantees solc reports the expression's ACTUAL type
# in its diagnostic. Probing with `uint256` instead would silently widen a `uint96` or a
# `uint8` and the debugger would lie about the type.
PROBE_STRUCT = "__SevmProbe"
PROBE_DECL = f"struct {PROBE_STRUCT} {{ uint8 __sevm_x; }}"
PROBE_TYPE = f"{PROBE_STRUCT} memory"

# solc phrases the mismatch two different ways depending on where it is caught.
_TYPE_ERROR = re.compile(
    r"(?:Return argument type|Type)\s+(.+?)\s+is not implicitly convertible to expected type"
)
_VOID_ERROR = re.compile(
    r"(?:Different number of arguments in return statement"
    r"|Type tuple\(\) is not implicitly convertible)"
)

# solc type string -> the type we can actually declare and abi-decode.
_TYPE_FIXUPS = {
    "address payable": "address",
    "bool": "bool",
    "string": "string memory",
    "bytes": "bytes memory",
}


class EvalError(RuntimeError):
    """The expression could not be compiled or it reverted."""


@dataclass
class EvalResult:
    expression: str
    type_name: str  # as declared, e.g. "string memory"
    abi_type: str  # as abi-decoded, e.g. "string"
    value: Any
    display: str
    raw: bytes = b""
    gas_used: int = 0
    compile_ms: float = 0.0
    kept: bool = False
    void: bool = False

    def __str__(self) -> str:
        return self.display


def _normalise_type(solc_type: str) -> str:
    """Map a solc type string onto something declarable in a `returns (...)` clause."""
    t = solc_type.strip()
    t = re.sub(r"\s+", " ", t)
    # solc appends storage/memory/calldata location and pointer-ness to reference types.
    t = re.sub(r"\s+(storage|memory|calldata)(\s+(ref|pointer|slice))?$", "", t)
    if t in _TYPE_FIXUPS:
        return _TYPE_FIXUPS[t]
    if t.startswith("int_const"):
        return "int256" if "-" in t else "uint256"
    if t.startswith("rational_const"):
        return "uint256"
    if t.startswith("literal_string"):
        return "string memory"
    if t.startswith("contract "):
        return "address"
    if t.startswith("enum "):
        return "uint256"
    if t.startswith("type("):
        raise EvalError(f"cannot display a type expression ({t})")
    if t.startswith("mapping("):
        raise EvalError("cannot display a whole mapping; index it with a key")
    if t.startswith("tuple("):
        raise EvalError("expression returns multiple values; evaluate them one at a time")
    if t.startswith("function "):
        raise EvalError("cannot display a function reference; call it instead")
    if t.endswith("]") or t.startswith("struct "):
        # Arrays and structs must be returned from memory.
        base = re.sub(r"^struct\s+", "", t)
        return f"{base} memory"
    return t


def _abi_type_of(declared: str) -> str:
    return (
        declared.replace(" memory", "")
        .replace(" calldata", "")
        .replace(" storage", "")
        .strip()
    )


def _inject(
    source: str,
    expression: str,
    return_type: str | None,
    contract_range: tuple[int, int] = (-1, -1),
    probe: bool = False,
    parameters: str = "",
) -> str:
    """Add the eval function just inside the target contract's closing brace.

    `contract_range` comes from the AST. Falling back to the last `}` in the file is
    wrong whenever a file declares more than one contract, which is the common case.
    """
    start, end = contract_range
    if start >= 0 and 0 < end <= len(source) and source[end - 1] == "}":
        close = end - 1
    else:
        close = source.rstrip().rfind("}")
    if close < 0:
        raise EvalError("cannot locate the contract body to inject into")
    if return_type is None:
        body = f"    function {EVAL_FUNCTION}({parameters}) public payable {{ {expression}; }}\n"
    else:
        body = (
            f"    function {EVAL_FUNCTION}({parameters}) public payable returns ({return_type})"
            f" {{ return ({expression}); }}\n"
        )
    if probe:
        body = f"    {PROBE_DECL}\n" + body
    return source[:close] + "\n" + body + source[close:]


def _format_value(value: Any, abi_type: str) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str) and abi_type == "string":
        return f'"{value}"'
    if abi_type == "address" and isinstance(value, str):
        return value
    if isinstance(value, bytes):
        return "0x" + value.hex()
    if isinstance(value, (list, tuple)):
        return "[" + ", ".join(_format_value(v, "") for v in value) + "]"
    if isinstance(value, int):
        # Show an ether reading only in the range where wei is a plausible reading:
        # 0.001 ether up to a billion. Above that the number is a hash, an address cast,
        # or type(uintN).max, and "721457446580647764635779334144 ether" is noise.
        if abi_type.startswith("uint") and 10**15 <= value < 10**27:
            whole, frac = divmod(value, 10**18)
            if frac == 0:
                return f"{value} ({whole} ether)"
            return f"{value} ({value / 10**18:.6f} ether)"
        return str(value)
    return str(value)


class Evaluator:
    """Compiles and runs Solidity expressions against a paused VM."""

    def __init__(self, project: Project) -> None:
        self.project = project
        self._code_cache: dict[tuple[str, str, str], tuple[bytes, str]] = {}
        self._type_cache: dict[tuple[str, str, str], str | None] = {}
        self.compile_count = 0

    # -- compilation --------------------------------------------------------

    def _sources_with(
        self,
        artifact: Artifact,
        expression: str,
        return_type: str | None,
        probe: bool = False,
        parameters: str = "",
    ) -> dict[str, str]:
        sources = {key: src.text for key, src in self.project.sources.items()}
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
            output_selection={"*": {"*": ["evm.deployedBytecode.object"]}},
            remappings=self.project.remappings or None,
        )

    def _infer_type(
        self, artifact: Artifact, expression: str, parameters: str = ""
    ) -> str | None:
        """Ask solc what the expression's type is, by making it complain.

        Compiles the expression as a return of an unreachable struct type, which nothing
        implicitly converts to, so the diagnostic always names the true type. Returns None
        for a void expression (a bare call), which is compiled as a statement instead.
        """
        key = (artifact.qualified_name, expression, parameters)
        if key in self._type_cache:
            return self._type_cache[key]
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
        self, artifact: Artifact, expression: str, parameters: str = ""
    ) -> tuple[bytes, str | None]:
        # The parameter list is part of the key, not the values it carries: the same
        # expression over the same locals compiles once and is reused at every stop.
        key = (artifact.qualified_name, expression, parameters)
        cached = self._code_cache.get(key)
        if cached is not None:
            code, declared = cached
            return code, (declared or None)

        return_type = self._infer_type(artifact, expression, parameters)
        try:
            out = self._try_compile(
                artifact, expression, return_type, parameters=parameters
            )
        except CompileError as exc:
            raise EvalError(_clean_solc_error(str(exc), expression)) from exc

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

        Runs on the VM thread while the hook is parked. Effects are reverted unless
        `keep` is set, which is gdb's `call` verb: evaluate and commit.
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
        bound = bindings_for(expression, bindings)
        parameters = _parameter_list(bound)

        started = time.time()
        code, return_type = self._compiled(artifact, expression, parameters)
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
        parameters = _parameter_list(bindings_for(expression, bindings))
        return self._infer_type(artifact, expression, parameters) or "void"


def _build_message(
    frame: Any, computation: Any, code: bytes, data: bytes = EVAL_SELECTOR
) -> Any:
    """A message that mirrors the paused frame so `msg.*` reads truthfully.

    `should_transfer_value` is off: we want `msg.value` to report the paused frame's
    value without moving ether a second time.
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
            return f"{message} (in `{expression}`)"
    return f"could not compile `{expression}`"


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
