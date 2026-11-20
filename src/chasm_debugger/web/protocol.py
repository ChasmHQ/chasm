"""Transport bounds and exact request validation for the web worker."""

from __future__ import annotations

import re
from typing import Any

PROTOCOL = "chasm-web-debugger/1"
MAX_REQUEST_BYTES = 262_144
MAX_RESPONSE_BYTES = 16_777_216
MAX_ITEMS = 128
MAX_TEXT = 8192
MAX_MEMORY = 16 * 1024 * 1024
MAX_SAFE_INTEGER = (1 << 53) - 1


class WebError(ValueError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


def fields(
    params: dict[str, Any], allowed: set[str], required: set[str] | None = None
) -> None:
    if set(params) - allowed:
        raise WebError("invalid_params", "unsupported parameter fields")
    if required and required - set(params):
        raise WebError("invalid_params", "missing required parameter fields")


def text(params: dict[str, Any], key: str, *, limit: int = MAX_TEXT) -> str:
    value = params.get(key)
    if not isinstance(value, str) or not value or len(value) > limit or "\x00" in value:
        raise WebError(
            "invalid_params",
            f"{key} must be a nonempty string of at most {limit} characters",
        )
    return value


def choice(params: dict[str, Any], key: str, options: set[str]) -> str:
    value = text(params, key, limit=64)
    if value not in options:
        raise WebError("invalid_params", f"unsupported {key}")
    return value


def boolean(params: dict[str, Any], key: str, default: bool = False) -> bool:
    value = params.get(key, default)
    if type(value) is not bool:
        raise WebError("invalid_params", f"{key} must be a boolean")
    return value


def integer(
    value: Any, key: str, maximum: int, *, minimum: int = 0, strings: bool = False
) -> int:
    if strings and isinstance(value, str) and re.fullmatch(r"[0-9]{1,78}", value):
        value = int(value)
    if type(value) is not int or not minimum <= value <= maximum:
        raise WebError(
            "invalid_params", f"{key} must be an integer between {minimum} and {maximum}"
        )
    return value


def word(value: Any, key: str = "value", maximum: int = (1 << 256) - 1) -> int:
    if not isinstance(value, str) or not re.fullmatch(
        r"(?:0x[0-9a-fA-F]{1,64}|[0-9]{1,78})", value
    ):
        raise WebError(
            "invalid_params", f"{key} must be an unsigned decimal or 0x-hex string"
        )
    return integer(int(value, 16 if value.startswith("0x") else 10), key, maximum)


def hex_data(
    value: Any, key: str, *, size: int | None = None, maximum: int = 65_536
) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"0x(?:[0-9a-fA-F]{2})*", value):
        raise WebError("invalid_params", f"{key} must be 0x-prefixed even-length hex")
    length = (len(value) - 2) // 2
    if length > maximum or (size is not None and length != size):
        raise WebError("invalid_params", f"invalid {key} byte length")
    return value


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise WebError("invalid_request", "duplicate JSON object key")
        result[key] = value
    return result


def reject_constant(value: str) -> None:
    raise WebError("invalid_request", "non-finite JSON numbers are forbidden")


def json_value(value: Any, *, bounded: bool = True) -> Any:
    paths: list[str] = []
    nodes = 0

    def convert(item: Any, path: str, depth: int) -> Any:
        nonlocal nodes
        nodes += 1
        if bounded and (depth > 20 or nodes > 32_768):
            paths.append(path)
            return None
        if isinstance(item, bool) or item is None:
            return item
        if isinstance(item, int):
            return str(item) if abs(item) > MAX_SAFE_INTEGER else item
        if isinstance(item, (bytes, bytearray)):
            item = "0x" + bytes(item).hex()
        if isinstance(item, str):
            if bounded and len(item) > MAX_TEXT:
                paths.append(path)
                return item[:MAX_TEXT] + " [truncated]"
            return item
        if isinstance(item, dict):
            entries = list(item.items())
            if bounded and len(entries) > MAX_ITEMS:
                paths.append(path)
                entries = entries[:MAX_ITEMS]
            converted = {
                str(key): convert(child, f"{path}.{key}", depth + 1)
                for key, child in entries
            }
            if converted.get("decimal", False) is None and isinstance(
                converted.get("hex"), str
            ):
                encoded_word = converted["hex"]
                if re.fullmatch(r"0x[0-9a-fA-F]{64}", encoded_word):
                    converted["decimal"] = str(int(encoded_word, 16))
            return converted
        if isinstance(item, (tuple, list)):
            if bounded and len(item) > MAX_ITEMS:
                paths.append(path)
                item = item[:MAX_ITEMS]
            return [
                convert(child, f"{path}[{index}]", depth + 1)
                for index, child in enumerate(item)
            ]
        raise WebError("operation_failed", "debugger returned a non-JSON value")

    result = convert(value, "$", 0)
    if paths and isinstance(result, dict):
        result["truncated"] = True
        result["truncation"] = {
            "paths": paths[:MAX_ITEMS],
            "limit": MAX_ITEMS,
            "string_limit": MAX_TEXT,
        }
    return result
