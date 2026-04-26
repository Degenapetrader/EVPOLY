#!/usr/bin/env python3
"""EVPOLY setup doctor.

Checks the current env against the baseline remote/runtime credentials that a
healthy EVPOLY setup should have, regenerates anything onboarding can provide,
and reports any remaining manual fields that still need user input.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any, Dict, List

import remote_onboard as ro


BASELINE_GENERATED = [
    (
        "EVPOLY_RELAYER_REMOTE_SIGNER_TOKEN",
        "Relayer Submit Signer Token",
        "Remote signer token used only for relayer submit fallback.",
    ),
    (
        "EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN",
        "Remote Discovery Token",
        "Shared discovery token used for remote timeframe market discovery.",
    ),
    (
        "EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN",
        "Premarket Remote Token",
        "Premarket remote alpha token.",
    ),
    (
        "EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN",
        "Endgame Remote Token",
        "Endgame remote alpha token.",
    ),
    (
        "EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN",
        "EVSnipe Discovery Token",
        "EVSnipe discovery token.",
    ),
    (
        "EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN",
        "EVcurve Remote Token",
        "EVcurve remote alpha token.",
    ),
]


def _env_value(env_path: Path, key: str) -> str:
    return ro._read_env_value(str(env_path), key).strip()


def _status_item(key: str, label: str, status: str, message: str) -> Dict[str, Any]:
    return {
        "key": key,
        "label": label,
        "status": status,
        "message": message,
    }


def _missing_sentence(labels: List[str]) -> str:
    if not labels:
        return ""
    if len(labels) == 1:
        return labels[0]
    if len(labels) == 2:
        return f"{labels[0]} and {labels[1]}"
    return ", ".join(labels[:-1]) + f", and {labels[-1]}"


def _parse_signature_type(raw: str) -> int:
    try:
        value = int((raw or "").strip())
    except Exception:
        return 0
    return value if value in (0, 1, 2) else 0


def _run_remote_onboard(env_path: Path, private_key: str, signature_type: int, proxy_wallet: str) -> str:
    script_path = Path(__file__).resolve().parent / "remote_onboard.py"
    command = [
        sys.executable,
        str(script_path),
        "--private-key",
        private_key,
        "--signature-type",
        str(signature_type),
        "--write-env-file",
        str(env_path),
        "--skip-approvals",
        "--skip-retention-cron",
    ]
    if signature_type in (1, 2):
        command.extend(["--proxy-wallet", proxy_wallet])
    completed = subprocess.run(
        command,
        capture_output=True,
        text=True,
        check=False,
    )
    stdout = completed.stdout.strip()
    stderr = completed.stderr.strip()
    if completed.returncode != 0:
        detail = stderr or stdout or f"exit code {completed.returncode}"
        raise RuntimeError(f"remote onboarding failed: {detail}")
    return stdout


def _collect_audit(env_path: Path) -> Dict[str, Any]:
    items: List[Dict[str, Any]] = []
    blocking_missing_labels: List[str] = []
    manual_missing_labels: List[str] = []
    generated_missing_keys: List[str] = []

    private_key = _env_value(env_path, "POLY_PRIVATE_KEY")
    signature_type = _parse_signature_type(_env_value(env_path, "POLY_SIGNATURE_TYPE"))
    proxy_wallet = _env_value(env_path, "POLY_PROXY_WALLET_ADDRESS")
    relayer_key = _env_value(env_path, "RELAYER_API_KEY")
    relayer_address = _env_value(env_path, "RELAYER_API_KEY_ADDRESS")

    if not private_key:
        items.append(
            _status_item(
                "POLY_PRIVATE_KEY",
                "Private Key",
                "missing_user",
                "Enter POLY_PRIVATE_KEY in .env before running Setup Doctor again.",
            )
        )
        blocking_missing_labels.append("Private Key")
        manual_missing_labels.append("Private Key")
    else:
        try:
            ro.Account.from_key(private_key)
        except Exception:
            items.append(
                _status_item(
                    "POLY_PRIVATE_KEY",
                    "Private Key",
                    "missing_user",
                    "POLY_PRIVATE_KEY is invalid. Replace it before running Setup Doctor again.",
                )
            )
            blocking_missing_labels.append("Private Key")
            manual_missing_labels.append("Private Key")

    if signature_type in (1, 2) and not proxy_wallet:
        items.append(
            _status_item(
                "POLY_PROXY_WALLET_ADDRESS",
                "Proxy Wallet",
                "missing_user",
                "Proxy/Safe mode requires POLY_PROXY_WALLET_ADDRESS in .env.",
            )
        )
        blocking_missing_labels.append("Proxy Wallet")
        manual_missing_labels.append("Proxy Wallet")

    if not relayer_key:
        items.append(
            _status_item(
                "RELAYER_API_KEY",
                "Relayer API Key",
                "missing_user",
                "Get RELAYER_API_KEY from https://polymarket.com/settings?tab=api-keys and add it to .env. EVPOLY can still use remote signer fallback where supported.",
            )
        )
        manual_missing_labels.append("Relayer API Key")

    if not relayer_address:
        items.append(
            _status_item(
                "RELAYER_API_KEY_ADDRESS",
                "Relayer API Key Address",
                "missing_user",
                "Get RELAYER_API_KEY_ADDRESS from https://polymarket.com/settings?tab=api-keys and add it to .env. EVPOLY can still use remote signer fallback where supported.",
            )
        )
        manual_missing_labels.append("Relayer API Key Address")

    for key, label, message in BASELINE_GENERATED:
        if not _env_value(env_path, key):
            items.append(
                _status_item(
                    key,
                    label,
                    "missing_generated",
                    f"{message} Setup Doctor will regenerate it from onboarding when available.",
                )
            )
            generated_missing_keys.append(key)

    if not items:
        items.append(
            _status_item(
                "setup_ready",
                "Setup",
                "ok",
                "All baseline setup fields are present.",
            )
        )

    return {
        "items": items,
        "blocking_missing_labels": blocking_missing_labels,
        "manual_missing_labels": manual_missing_labels,
        "generated_missing_keys": generated_missing_keys,
        "private_key": private_key,
        "signature_type": signature_type,
        "proxy_wallet": proxy_wallet,
    }


def _mark_fixed_items(items: List[Dict[str, Any]], fixed_keys: List[str]) -> None:
    for item in items:
        if item["key"] in fixed_keys and item["status"] in {"missing_generated", "ok"}:
            item["status"] = "fixed"
            item["message"] = f'{item["label"]} regenerated automatically.'


def _mark_unfixed_generated(items: List[Dict[str, Any]], missing_keys: List[str]) -> None:
    for item in items:
        if item["key"] in missing_keys and item["status"] == "missing_generated":
            item["status"] = "failed"
            item["message"] = (
                f'{item["label"]} is still missing after doctor regeneration. '
                "Rerun onboarding or inspect the remote onboarding service."
            )


def _popup_for_needs_you(audit: Dict[str, Any], relayer_only: bool) -> Dict[str, str]:
    if audit["blocking_missing_labels"]:
        missing = _missing_sentence(audit["blocking_missing_labels"])
        return {
            "title": f"Missing {missing}",
            "body": f"Add {missing} to .env, then run Setup Doctor again.",
        }
    if relayer_only:
        return {
            "title": "Add Relayer Credentials",
            "body": (
                "Get RELAYER_API_KEY and RELAYER_API_KEY_ADDRESS from "
                "https://polymarket.com/settings?tab=api-keys and add them to .env. "
                "EVPOLY can still use remote signer fallback where supported."
            ),
        }
    missing = _missing_sentence(audit["manual_missing_labels"])
    if missing:
        return {
            "title": "Finish Setup Doctor",
            "body": f"Setup Doctor finished, but {missing} still needs manual input.",
        }
    return {
        "title": "Finish Setup Doctor",
        "body": "Setup Doctor finished, but some baseline remote credentials are still missing.",
    }


def run_doctor(env_path: Path) -> Dict[str, Any]:
    env_path = env_path.expanduser().resolve()
    repo_root = Path(__file__).resolve().parents[1]
    ro._ensure_env_file(str(env_path), repo_root)

    initial = _collect_audit(env_path)
    fixed_keys: List[str] = []

    if initial["generated_missing_keys"] and not initial["blocking_missing_labels"]:
        _run_remote_onboard(
            env_path,
            initial["private_key"],
            initial["signature_type"],
            initial["proxy_wallet"],
        )

    final = _collect_audit(env_path)
    for key, _, _ in BASELINE_GENERATED:
        if key in initial["generated_missing_keys"] and not _env_value(env_path, key):
            continue
        if key in initial["generated_missing_keys"] and _env_value(env_path, key):
            fixed_keys.append(key)

    _mark_fixed_items(final["items"], fixed_keys)
    _mark_unfixed_generated(final["items"], final["generated_missing_keys"])

    relayer_only = (
        not final["blocking_missing_labels"]
        and not final["generated_missing_keys"]
        and any(
            item["key"] in {"RELAYER_API_KEY", "RELAYER_API_KEY_ADDRESS"}
            for item in final["items"]
            if item["status"] == "missing_user"
        )
    )

    if final["manual_missing_labels"] or final["generated_missing_keys"]:
        status = "needs_you"
        popup = _popup_for_needs_you(final, relayer_only)
    elif fixed_keys:
        status = "fixed"
        popup = {
            "title": "Setup Doctor Fixed Missing Setup",
            "body": "Setup Doctor refreshed the baseline relayer signer setup and regenerated any missing remote credentials.",
        }
    else:
        status = "ready"
        popup = None

    return {
        "status": status,
        "items": final["items"],
        "fixed_count": sum(1 for item in final["items"] if item["status"] == "fixed"),
        "missing_user_count": sum(1 for item in final["items"] if item["status"] == "missing_user"),
        "popup": popup,
    }


def _print_human(result: Dict[str, Any]) -> None:
    print("EVPOLY Setup Doctor")
    print(f"status={result['status']}")
    print(f"fixed={result['fixed_count']}")
    print(f"need_input={result['missing_user_count']}")
    print()
    for item in result["items"]:
        print(f"- [{item['status']}] {item['label']}: {item['message']}")
    if result.get("popup"):
        print()
        print(f"note={result['popup']['title']}")
        print(result["popup"]["body"])


def main() -> int:
    parser = argparse.ArgumentParser(description="EVPOLY missing-setup doctor")
    parser.add_argument(
        "--env-file",
        default=".env",
        help="Target env file to inspect and update (default: .env)",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Print JSON output instead of human-readable text.",
    )
    args = parser.parse_args()

    try:
        result = run_doctor(Path(args.env_file))
    except Exception as exc:
        error_result = {
            "status": "failed",
            "items": [],
            "fixed_count": 0,
            "missing_user_count": 0,
            "popup": {
                "title": "Doctor failed",
                "body": str(exc),
            },
        }
        if args.json:
            print(json.dumps(error_result, indent=2))
        else:
            _print_human(error_result)
        return 1

    if args.json:
        print(json.dumps(result, indent=2))
    else:
        _print_human(result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
