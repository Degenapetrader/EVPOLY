#!/usr/bin/env python3
"""EVPOLY setup doctor.

Checks the current env against the baseline public V2 runtime fields that a
healthy EVPOLY setup should have, confirms alpha self-onboarding posture, and
reports any remaining manual fields that still need user input.
"""

from __future__ import annotations

import argparse
import json
import shutil
import string
from pathlib import Path
from typing import Any, Dict, List


def _env_value(env_path: Path, key: str) -> str:
    if not env_path.exists():
        return ""
    needle = f"{key}="
    for line in env_path.read_text(encoding="utf-8").splitlines():
        if line.startswith(needle):
            return line.split("=", 1)[1].strip()
    return ""


def _ensure_env_file(env_path: Path, repo_root: Path) -> None:
    if env_path.exists():
        return
    env_path.parent.mkdir(parents=True, exist_ok=True)
    for seed_name in (".env.example", ".env.full.example"):
        seed = repo_root / seed_name
        if seed.exists():
            shutil.copyfile(seed, env_path)
            return
    env_path.write_text("", encoding="utf-8")


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


def _parse_env_bool(raw: str, default: bool) -> bool:
    normalized = (raw or "").strip().lower()
    if normalized in {"1", "true", "yes", "on"}:
        return True
    if normalized in {"0", "false", "no", "off"}:
        return False
    return default


def _private_key_looks_valid(raw: str) -> bool:
    value = raw.strip()
    if value.startswith("0x"):
        value = value[2:]
    return len(value) == 64 and all(ch in string.hexdigits for ch in value)


def _collect_audit(env_path: Path) -> Dict[str, Any]:
    items: List[Dict[str, Any]] = []
    blocking_missing_labels: List[str] = []
    manual_missing_labels: List[str] = []

    private_key = _env_value(env_path, "POLY_PRIVATE_KEY")
    signature_type = _parse_signature_type(_env_value(env_path, "POLY_SIGNATURE_TYPE"))
    proxy_wallet = _env_value(env_path, "POLY_PROXY_WALLET_ADDRESS")
    relayer_key = _env_value(env_path, "RELAYER_API_KEY")
    relayer_address = _env_value(env_path, "RELAYER_API_KEY_ADDRESS")
    alpha_key = _env_value(env_path, "EVPOLY_ALPHA_KEY")
    alpha_auto_onboard = _parse_env_bool(_env_value(env_path, "EVPOLY_ALPHA_AUTO_ONBOARD"), True)

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
        if not _private_key_looks_valid(private_key):
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

    if alpha_key:
        items.append(
            _status_item(
                "EVPOLY_ALPHA_KEY",
                "Alpha Access",
                "ok",
                "EVPOLY_ALPHA_KEY is already present.",
            )
        )
    elif alpha_auto_onboard:
        items.append(
            _status_item(
                "EVPOLY_ALPHA_KEY",
                "Alpha Access",
                "ok",
                (
                    "EVPOLY_ALPHA_KEY is blank; runtime will auto-register it on first start "
                    "when POLY_PROXY_WALLET_ADDRESS and the official builder code are present."
                ),
            )
        )
    else:
        items.append(
            _status_item(
                "EVPOLY_ALPHA_KEY",
                "Alpha Access",
                "missing_user",
                "Set EVPOLY_ALPHA_KEY manually or set EVPOLY_ALPHA_AUTO_ONBOARD=true.",
            )
        )
        manual_missing_labels.append("Alpha Access")

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
    }


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
    _ensure_env_file(env_path, repo_root)

    final = _collect_audit(env_path)

    relayer_only = (
        not final["blocking_missing_labels"]
        and any(
            item["key"] in {"RELAYER_API_KEY", "RELAYER_API_KEY_ADDRESS"}
            for item in final["items"]
            if item["status"] == "missing_user"
        )
    )

    if final["manual_missing_labels"]:
        status = "needs_you"
        popup = _popup_for_needs_you(final, relayer_only)
    else:
        status = "ready"
        popup = None

    return {
        "status": status,
        "items": final["items"],
        "fixed_count": 0,
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
