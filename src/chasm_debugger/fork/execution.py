from __future__ import annotations

import json
from typing import Any

from .validation import (
    ForkError,
    address,
    data,
    hash32,
    integer,
    logs,
    native_request,
    obj,
    required,
    sequence,
)


def drain_transaction(chain: Any, request: dict[str, Any]) -> dict[str, Any]:
    native_request(request)
    try:
        chain.transact_json(json.dumps(request, separators=(",", ":"), sort_keys=True))
        for _ in range(10000):
            event = obj(chain.wait(timeout=30.0), "native event")
            kind = event.get("type")
            if kind == "paused":
                chain.resume()
            elif kind == "finished":
                return execution_receipt(event)
            elif kind == "host_call":
                raise ForkError(
                    "host calls are forbidden during strict historical execution"
                )
            elif kind == "failed":
                raise ForkError(
                    "strict native transaction failed (provider details redacted)"
                )
            else:
                raise ForkError("unexpected native transaction event")
    except ForkError:
        raise
    except Exception:
        raise ForkError("native transaction failed (endpoint details redacted)") from None
    raise ForkError("too many native pauses during noninteractive execution")


def execution_receipt(event: dict[str, Any]) -> dict[str, Any]:
    if type(event.get("success")) is not bool:
        raise ForkError("native receipt has no boolean success")
    normalized_logs = []
    for item in sequence(required(event, "logs"), "native logs"):
        item = obj(item, "native log")
        topics = []
        for topic in item["topics"]:
            # The existing bridge emits quantity-form topics, unlike RPC DATA32.
            if (
                not isinstance(topic, str)
                or not topic.startswith("0x")
                or not 1 <= len(topic[2:]) <= 64
                or any(char not in "0123456789abcdef" for char in topic[2:])
            ):
                raise ForkError("invalid native log topic")
            topics.append(hash32("0x" + topic[2:].zfill(64), "native log topic"))
        payload = item["data"]
        if isinstance(payload, (bytes, bytearray)):
            payload = "0x" + bytes(payload).hex()
        normalized_logs.append(
            {
                "address": address(item["address"]),
                "topics": topics,
                "data": data(payload, "native log data"),
            }
        )
    created = required(event, "created_address")
    if created is not None:
        address(created, "native created_address")
    return {
        "status": int(event["success"]),
        "gas_used": integer(event.get("gas_used"), "native gas_used"),
        "logs": logs(normalized_logs),
        "contract_address": created,
    }


def compare_receipt(expected: dict[str, Any], actual: dict[str, Any]) -> dict[str, Any]:
    differences = {
        key: {"expected": expected[key], "actual": actual[key]}
        for key in ("status", "gas_used", "logs", "contract_address")
        if expected[key] != actual[key]
    }
    return {
        "identity": expected["identity"],
        "matched": not differences,
        "differences": differences,
        "actual": actual,
    }


def export_witness(chain: Any) -> dict[str, Any]:
    try:
        result = obj(json.loads(chain.export_fork()), "native fork witness")
    except Exception:
        raise ForkError(
            "native witness export failed (endpoint details redacted)"
        ) from None
    if not result:
        raise ForkError("native witness export is empty")
    return result
