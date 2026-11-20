from __future__ import annotations

import copy
import json
import os
from pathlib import Path
from typing import Any

from .plans import BaselineResult, ForkPlan, ReplayPlan
from .validation import ForkError, obj

CAPTURE_FORMAT = "chasm-fork-capture"
CAPTURE_VERSION = 1
_FORBIDDEN_KEYS = frozenset(
    {"rpc_url", "url", "endpoint", "authorization", "password", "api_key", "token"}
)


def credential_free(value: Any) -> None:
    if isinstance(value, dict):
        for key, item in value.items():
            if not isinstance(key, str) or key.lower() in _FORBIDDEN_KEYS:
                raise ForkError(
                    "capture contains a forbidden endpoint or credential field"
                )
            credential_free(item)
    elif isinstance(value, list):
        for item in value:
            credential_free(item)
    elif isinstance(value, str):
        if "://" in value:
            raise ForkError("capture cannot contain endpoint URLs")
    elif value is not None and type(value) not in (int, bool):
        raise ForkError("capture contains a non-JSON value")


def capture_bundle(
    plan: ForkPlan, baseline: BaselineResult, *, witness: dict[str, Any] | None = None
) -> dict[str, Any]:
    remote = witness if witness is not None else baseline.witness
    if not obj(remote, "remote-state witness"):
        raise ForkError("capture requires a nonempty remote-state witness")
    if baseline.report.get("identity") != plan.identity:
        raise ForkError("baseline belongs to a different plan")
    bundle = {
        "format": CAPTURE_FORMAT,
        "version": CAPTURE_VERSION,
        "plan": plan.metadata,
        "baseline": copy.deepcopy(baseline.report),
        "witness": copy.deepcopy(remote),
    }
    credential_free(bundle)
    return bundle


def save_capture(
    path: str | os.PathLike[str],
    plan: ForkPlan,
    baseline: BaselineResult,
    *,
    witness: dict[str, Any] | None = None,
    overwrite: bool = False,
) -> None:
    bundle = capture_bundle(plan, baseline, witness=witness)
    serialized = (
        json.dumps(bundle, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n"
    )
    with Path(path).open(
        "w" if overwrite else "x", encoding="utf-8", newline="\n"
    ) as handle:
        handle.write(serialized)


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ForkError("duplicate JSON key in capture")
        result[key] = value
    return result


def load_capture(path: str | os.PathLike[str]) -> ReplayPlan | ForkPlan:
    try:
        with Path(path).open(encoding="utf-8") as handle:
            bundle = obj(json.load(handle, object_pairs_hook=_unique_object), "capture")
    except (json.JSONDecodeError, UnicodeError):
        raise ForkError("invalid capture JSON") from None
    if set(bundle) != {"format", "version", "plan", "baseline", "witness"}:
        raise ForkError("capture has missing or unsupported fields")
    if (
        bundle["format"] not in {CAPTURE_FORMAT, "sevm-fork-capture"}
        or type(bundle["version"]) is not int
        or bundle["version"] != CAPTURE_VERSION
    ):
        raise ForkError("unsupported fork capture format/version")
    credential_free(bundle)
    witness = obj(bundle["witness"], "remote-state witness")
    if not witness:
        raise ForkError("offline capture is missing its remote-state witness")
    metadata = obj(bundle["plan"], "capture plan")
    baseline = obj(bundle["baseline"], "capture baseline")
    cls = ReplayPlan if metadata.get("kind") == "replay" else ForkPlan
    plan = cls(metadata, witness=witness, baseline_report=baseline)
    if baseline.get("identity") != plan.identity:
        raise ForkError("captured baseline identity differs from plan")
    return plan
