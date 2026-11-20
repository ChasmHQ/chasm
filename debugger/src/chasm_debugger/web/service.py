"""Fixed web operations over the shared debugger driver and command processor."""

from __future__ import annotations

import os
import re
import threading
from collections.abc import Callable
from typing import Any

from .. import __version__
from .._revm import revm_version
from ..assembly import has_builtin_head
from ..mcp_server.driver import DebugDriver
from .protocol import (
    MAX_MEMORY,
    MAX_SAFE_INTEGER,
    PROTOCOL,
    WebError,
    boolean,
    choice,
    fields,
    hex_data,
    integer,
    json_value,
    text,
    word,
)

MODES = {"offline", "replay", "fork", "foundry"}
MAX_LAUNCH_BREAKPOINTS = 32
OUTPUT_LINES = 200
OUTPUT_LINE = 2048
_STATE_CHEATS = frozenset(
    {
        "warp",
        "roll",
        "fee",
        "chainId",
        "coinbase",
        "deal",
        "etch",
        "store",
        "load",
        "prank",
        "startPrank",
        "stopPrank",
        "label",
        "difficulty",
        "prevrandao",
        "setNonce",
        "getNonce",
    }
)


class WebService:
    def __init__(self) -> None:
        self.driver = DebugDriver()
        self._lock = threading.RLock()
        self._active = False
        self.closed = False
        self._rpc_env: str | None = None
        self._storage_slots: list[int] = [0]

    def message(self, error: Exception) -> str:
        message = str(error)
        endpoint = os.environ.get(self._rpc_env or "", "")
        if endpoint:
            message = message.replace(endpoint, "[RPC endpoint redacted]")
        return re.sub(
            r"\b[a-zA-Z][a-zA-Z0-9+.-]*://[^\s\"'<>]+", "[endpoint redacted]", message
        )[:2048]

    def dispatch(self, request: Any) -> dict[str, Any]:
        with self._lock:
            if not isinstance(request, dict) or set(request) != {
                "id",
                "method",
                "params",
            }:
                raise WebError(
                    "invalid_request",
                    "request must contain exactly id, method and params",
                )
            request_id = request["id"]
            if not isinstance(request_id, str) or not request_id or len(request_id) > 256:
                raise WebError(
                    "invalid_request",
                    "id must be a nonempty string of at most 256 characters",
                )
            method, params = request["method"], request["params"]
            if not isinstance(method, str) or not isinstance(params, dict):
                raise WebError(
                    "invalid_request", "method must be a string and params an object"
                )
            handlers: dict[str, Callable[[dict[str, Any]], dict[str, Any]]] = {
                "hello": self.hello,
                "start": self.start,
                "snapshot": self.snapshot,
                "navigate": self.navigate,
                "breakpoint": self.breakpoint,
                "evaluate": self.evaluate,
                "mutate": self.mutate,
                "command": self.command,
                "export_capture": self.export_capture,
                "shutdown": self.shutdown,
            }
            handler = handlers.get(method)
            if handler is None:
                raise WebError("unknown_method", "unsupported web debugger method")
            if self.closed:
                raise WebError("operation_failed", "worker has shut down")
            if method not in {"hello", "start", "shutdown"} and not self._active:
                raise WebError("no_session", "start a debug session first")
            try:
                result = handler(params)
            except WebError:
                raise
            except Exception as exc:
                raise WebError("operation_failed", self.message(exc)) from None
            return json_value(result, bounded=method != "export_capture")

    def hello(self, params: dict[str, Any]) -> dict[str, Any]:
        fields(params, set())
        return {
            "protocol": PROTOCOL,
            "engine_version": revm_version(),
            "package_version": __version__,
            "capabilities": {"modes": sorted(MODES), "pause": False},
        }

    def start(self, params: dict[str, Any]) -> dict[str, Any]:
        mode = choice(params, "mode", MODES | {"local"})
        common = {"mode", "build_info_path"}
        allowed = {
            "offline": common | {"capture_path"},
            "replay": common | {"capture_path", "tx_hash", "rpc_env"},
            "fork": common
            | {
                "block_number",
                "rpc_env",
                "to",
                "caller",
                "data",
                "value",
                "gas",
                "nonce",
                "gas_price",
            },
            "foundry": {"mode", "target_path", "match", "pause_at_entry", "breakpoints"},
            "local": {"mode"},
        }
        fields(params, allowed[mode])
        if mode == "local":
            raise WebError(
                "unsupported",
                "local mode is not implemented; strict chain-id checks remain enabled",
            )
        for key in ("capture_path", "build_info_path", "target_path", "match"):
            if key in params:
                text(params, key, limit=4096)
        if "rpc_env" in params and not re.fullmatch(
            r"[A-Za-z_][A-Za-z0-9_]{0,127}", text(params, "rpc_env")
        ):
            raise WebError(
                "invalid_params",
                "rpc_env must name an environment variable, not an endpoint",
            )
        if "tx_hash" in params:
            hex_data(params["tx_hash"], "tx_hash", size=32)
        if mode in {"offline", "replay"}:
            offline = params.get("capture_path")
            if offline:
                if "tx_hash" in params or "rpc_env" in params:
                    raise WebError(
                        "invalid_params",
                        "capture replay cannot include tx_hash or rpc_env",
                    )
            elif mode == "offline" or not {"tx_hash", "rpc_env"} <= set(params):
                raise WebError(
                    "invalid_params", "provide capture_path or both tx_hash and rpc_env"
                )
        if mode == "fork":
            fields(
                params,
                allowed[mode],
                {"mode", "block_number", "rpc_env", "to", "caller", "nonce", "gas_price"},
            )
            integer(params["block_number"], "block_number", MAX_SAFE_INTEGER)
            integer(params["nonce"], "nonce", MAX_SAFE_INTEGER)
            for key in ("to", "caller"):
                hex_data(params[key], key, size=20)
            hex_data(params.get("data", "0x"), "data")
            word(params.get("value", "0"))
            word(params["gas_price"], "gas_price")
            integer(params.get("gas", 3_000_000), "gas", MAX_SAFE_INTEGER, minimum=1)
        if mode == "foundry" and not text(params, "target_path", limit=4096).endswith(
            ".sol"
        ):
            raise WebError(
                "invalid_params", "Foundry target_path must be a Solidity file"
            )
        if "pause_at_entry" in params and not isinstance(params["pause_at_entry"], bool):
            raise WebError("invalid_params", "pause_at_entry must be a boolean")
        launch_breakpoints = params.get("breakpoints", [])
        if (
            not isinstance(launch_breakpoints, list)
            or len(launch_breakpoints) > MAX_LAUNCH_BREAKPOINTS
            or any(
                not isinstance(spec, str)
                or not spec.strip()
                or len(spec) > 512
                or any(ord(char) < 32 for char in spec)
                for spec in launch_breakpoints
            )
        ):
            raise WebError(
                "invalid_params",
                f"breakpoints must be at most {MAX_LAUNCH_BREAKPOINTS} locations "
                "(File.sol:LINE, *PC or a function name)",
            )
        self._active = False
        self._rpc_env = params.get("rpc_env")
        self._storage_slots = [0]
        try:
            if mode in {"offline", "replay"}:
                self.driver.start_replay(
                    tx_hash=params.get("tx_hash"),
                    rpc_env=params.get("rpc_env"),
                    offline=params.get("capture_path"),
                    contracts=params.get("build_info_path"),
                )
            elif mode == "fork":
                self.driver.start_fork(
                    block_number=params["block_number"],
                    rpc_env=params["rpc_env"],
                    to=params["to"],
                    caller=params["caller"],
                    data=params.get("data", "0x"),
                    value=word(params.get("value", "0")),
                    gas=params.get("gas", 3_000_000),
                    gas_price=word(params["gas_price"], "gas_price"),
                    nonce=params["nonce"],
                    contracts=params.get("build_info_path"),
                )
            else:
                self.driver.start(
                    params["target_path"],
                    match=params.get("match"),
                    safe_foundry=True,
                    pause_at_entry=params.get("pause_at_entry", True),
                    breakpoints=launch_breakpoints,
                )
            self._active = True
            return self.snapshot({})
        except Exception as exc:
            self._active = False
            self.driver.stop()
            raise WebError("start_failed", self.message(exc)) from None

    def _pane(self, operation: Callable[[], dict[str, Any]]) -> dict[str, Any]:
        try:
            return json_value(operation())
        except Exception as exc:
            return {"error": {"code": "pane_unavailable", "message": self.message(exc)}}

    def snapshot(self, params: dict[str, Any]) -> dict[str, Any]:
        fields(params, {"memoryOffset", "storageSlots", "sourceStart", "aroundPc"})
        offset = integer(
            params.get("memoryOffset", "0"), "memoryOffset", MAX_MEMORY, strings=True
        )
        slots = self._storage_slots
        if "storageSlots" in params:
            raw_slots = params["storageSlots"]
            if not isinstance(raw_slots, list) or len(raw_slots) > 64:
                raise WebError(
                    "invalid_params",
                    "storageSlots must contain at most 64 decimal strings",
                )
            if any(not isinstance(slot, str) for slot in raw_slots):
                raise WebError(
                    "invalid_params", "storageSlots entries must be decimal strings"
                )
            slots = [
                integer(slot, "storageSlots", (1 << 256) - 1, strings=True)
                for slot in raw_slots
            ]
        around = None
        if "aroundPc" in params:
            around = integer(params["aroundPc"], "aroundPc", (1 << 32) - 1, strings=True)
        source_line = None
        if "sourceStart" in params:
            source_line = (
                integer(
                    params["sourceStart"],
                    "sourceStart",
                    MAX_SAFE_INTEGER,
                    minimum=1,
                    strings=True,
                )
                + 50
            )
        capabilities = self.driver.capabilities()
        source = (
            self._pane(
                lambda: self.driver.read_source(around_line=source_line, context=50)
            )
            if capabilities["source"]
            else {
                "available": False,
                "reason": capabilities.get("reason", "source unavailable at this pc"),
            }
        )
        panes = {
            "stack": self._pane(lambda: self.driver.read_stack(limit=128)),
            "memory": self._pane(
                lambda: self.driver.read_memory(offset=offset, words=64)
            ),
            "storage": self._pane(
                lambda: self.driver.read_storage(slots=slots, limit=64)
            ),
            "frames": self._pane(self.driver.get_backtrace),
            "locals": self._pane(self.driver.get_locals)
            if capabilities["locals"]
            else {
                "available": False,
                "reason": capabilities.get("reason", "locals unavailable"),
            },
            "arguments": self._pane(lambda: self.driver.get_arguments(max_calldata=4096)),
            "disassembly": self._pane(
                lambda: self.driver.disassemble(around_pc=around, before=32, after=64)
            ),
            "source": source,
            "calldata": self._pane(self.driver.read_calldata),
            "gas": self._pane(self.driver.get_gas_profile),
            "logs": self._pane(lambda: self.driver.get_logs(limit=128, data_limit=4096)),
            "diff": self._pane(self.driver.diff_since_last_stop),
            "output": self.output(),
        }
        status = self.driver.status()
        return {
            "status": status,
            "capabilities": capabilities,
            "panes": panes,
            "breakpoints": self.driver.list_breakpoints(structured=True),
            "fork": status.get("fork"),
        }

    def output(self) -> dict[str, Any]:
        """Compiler notices, launch breakpoints and how the run ended, bounded
        and with endpoints redacted. Raw process output is never included."""
        lines = [
            self.message(RuntimeError(line))[:OUTPUT_LINE]
            for line in self.driver.notices[-OUTPUT_LINES:]
        ]
        return {"lines": lines, "truncated": len(self.driver.notices) > OUTPUT_LINES}

    def navigate(self, params: dict[str, Any]) -> dict[str, Any]:
        fields(params, {"action", "count", "location"}, {"action"})
        action = choice(
            params,
            "action",
            {
                "continue",
                "step_opcode",
                "step_line",
                "next_line",
                "finish",
                "restart",
                "run_to",
            },
        )
        count = integer(params.get("count", 1), "count", 1000, minimum=1)
        if "count" in params and action not in {"step_opcode", "step_line", "next_line"}:
            raise WebError("invalid_params", "count applies only to step operations")
        if "location" in params and action != "run_to":
            raise WebError("invalid_params", "location applies only to run_to")
        if (
            action in {"step_line", "next_line"}
            and not self.driver.capabilities()["source"]
        ):
            raise WebError(
                "unsupported", "source stepping is unavailable in raw bytecode"
            )
        if action == "continue":
            self.driver.continue_execution()
        elif action == "step_opcode":
            self.driver.step_opcodes(count)
        elif action == "step_line":
            self.driver.step_lines(count)
        elif action == "next_line":
            self.driver.next_lines(count)
        elif action == "finish":
            self.driver.finish_frame()
        elif action == "restart":
            if not self.driver.capabilities()["restart"]:
                raise WebError("unsupported", "restart is not supported for this target")
            self.driver.restart()
        else:
            self.driver.run_to(location=text(params, "location"))
        return self.snapshot({})

    def breakpoint(self, params: dict[str, Any]) -> dict[str, Any]:
        action = choice(params, "action", {"add", "delete", "list", "opcode", "watch"})
        allowed = {
            "add": {"location", "condition"},
            "delete": {"id"},
            "list": set(),
            "opcode": {"opcode", "condition"},
            "watch": {"expression"},
        }
        fields(params, {"action"} | allowed[action])
        condition = text(params, "condition") if "condition" in params else None
        if action == "add":
            self.driver.set_breakpoint(text(params, "location"), condition=condition)
        elif action == "delete":
            self.driver.delete_breakpoint(
                integer(params.get("id"), "id", MAX_SAFE_INTEGER, minimum=1, strings=True)
            )
        elif action == "opcode":
            opcode = text(params, "opcode", limit=32)
            if not re.fullmatch(r"[A-Za-z0-9_]+", opcode):
                raise WebError("invalid_params", "invalid opcode mnemonic")
            self.driver.set_opcode_breakpoint(opcode, condition=condition)
        elif action == "watch":
            self.driver.set_watchpoint(text(params, "expression"))
        return {
            "snapshot": self.snapshot({}),
            "breakpoints": self.driver.list_breakpoints(structured=True),
        }

    def evaluate(self, params: dict[str, Any]) -> dict[str, Any]:
        fields(params, {"expression", "keep"}, {"expression"})
        expression, keep = text(params, "expression"), boolean(params, "keep")
        if not self.driver.capabilities()["solidity"]:
            raise WebError(
                "unsupported", "Solidity evaluation requires matching build-info"
            )
        return {
            "evaluation": self.driver.evaluate(expression, keep=keep),
            "snapshot": self.snapshot({}),
        }

    def mutate(self, params: dict[str, Any]) -> dict[str, Any]:
        target = choice(
            params, "target", {"storage", "memory", "stack", "gas", "pc", "variable"}
        )
        extra = {
            "storage": {"key"},
            "memory": {"offset"},
            "stack": {"index"},
            "gas": set(),
            "pc": set(),
            "variable": {"name"},
        }
        fields(params, {"target", "value"} | extra[target], {"value"})
        value_text = text(params, "value")
        if target == "memory":
            offset = integer(params.get("offset"), "offset", MAX_MEMORY, strings=True)
            data = hex_data(value_text, "value", maximum=4096)
            if offset + (len(data) - 2) // 2 > MAX_MEMORY:
                raise WebError("invalid_params", "memory write exceeds maximum offset")
            result = self.driver.write_memory(offset, data)
        elif target == "variable":
            name = text(params, "name", limit=128)
            if not re.fullmatch(r"[A-Za-z_$][A-Za-z0-9_$]*", name):
                raise WebError(
                    "invalid_params", "name must be a bare Solidity identifier"
                )
            if not self.driver.capabilities()["solidity"]:
                raise WebError(
                    "unsupported", "variable mutation requires matching build-info"
                )
            value = word(value_text)
            result = self.driver.command(f"set var {name} = {value}")
        elif target == "storage":
            slot, value = word(params.get("key"), "key"), word(value_text)
            result = self.driver.write_storage(slot, value)
            self._storage_slots = list(dict.fromkeys([slot, *self._storage_slots]))[:64]
        elif target == "stack":
            result = self.driver.set_stack_slot(
                integer(params.get("index"), "index", 1023, strings=True),
                word(value_text),
            )
        elif target == "gas":
            result = self.driver.set_gas(word(value_text, maximum=(1 << 64) - 1))
        else:
            result = self.driver.set_pc(word(value_text, maximum=(1 << 32) - 1))
        return {"mutation": result, "snapshot": self.snapshot({})}

    def command(self, params: dict[str, Any]) -> dict[str, Any]:
        fields(params, {"text"}, {"text"})
        command = text(params, "text").strip()
        if not command:
            raise WebError("invalid_params", "text must contain a command")
        cheat = re.fullmatch(
            r"vm\.([A-Za-z][A-Za-z0-9_]*)\s*\((.*)\)", command, re.DOTALL
        )
        if cheat:
            if cheat.group(1) not in _STATE_CHEATS:
                raise WebError(
                    "unsupported", "only allowlisted state-only vm cheats are supported"
                )
        elif not (
            command.split(None, 1)[0] in {"asm", "assembly", "yul"}
            or has_builtin_head(command)
        ):
            raise WebError(
                "unsupported", "command accepts only Yul/asm and allowlisted vm cheats"
            )
        if not cheat and has_builtin_head(command):
            command = f"asm {command}"
        result = self.driver.command(command)
        return {"command": result, "snapshot": self.snapshot({})}

    def export_capture(self, params: dict[str, Any]) -> dict[str, Any]:
        fields(params, set())
        return self.driver.export_capture()

    def shutdown(self, params: dict[str, Any]) -> dict[str, Any]:
        fields(params, set())
        self.driver.stop()
        self._active = False
        self.closed = True
        return {"shutdown": True}
