#!/usr/bin/env python3
"""EVPOLY remote signer self-onboarding helper.

Flow:
1) POST /onboard/start
2) Sign challenge message with wallet private key
3) POST /onboard/finish
4) Auto-write runtime signer/discovery config into an env file
5) For proxy/safe wallets, run approval bootstrap txs (unless skipped)
"""

import argparse
import json
import os
import secrets
import shlex
import string
import subprocess
import sys
from pathlib import Path
from typing import Any, Dict, Tuple

import requests
from eth_account import Account
from eth_account.messages import encode_defunct
from eth_utils import to_checksum_address

DEFAULT_REMOTE_SIGNER_URL = "https://im23e4zz3k.execute-api.eu-west-1.amazonaws.com/sign"
DEFAULT_ORDER_SIGNER_URL = "https://signer.evplus.ai/sign/order"
DEFAULT_ALPHA_BASE_URL = "https://alpha.evplus.ai"


def _normalize_base(api_base: str) -> str:
    value = api_base.strip().rstrip("/")
    if not value.startswith("http://") and not value.startswith("https://"):
        raise ValueError("api base must start with http:// or https://")
    if value.endswith("/sign"):
        value = value[: -len("/sign")]
    return value


def _normalize_wallet(wallet: str) -> str:
    value = wallet.strip()
    if not value:
        return ""
    if not value.startswith("0x"):
        value = "0x" + value
    if len(value) != 42:
        raise ValueError("invalid ethereum wallet address")
    if any(ch not in string.hexdigits for ch in value[2:]):
        raise ValueError("invalid ethereum wallet address")
    return to_checksum_address(value)


def _headers() -> Dict[str, str]:
    return {"content-type": "application/json", "accept": "application/json"}


def _coerce_bool(value: Any, default: bool) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        normalized = value.strip().lower()
        if normalized in {"1", "true", "yes", "on"}:
            return True
        if normalized in {"0", "false", "no", "off"}:
            return False
    return default


def _coerce_int(value: Any, default: int, minimum: int, maximum: int) -> int:
    try:
        parsed = int(str(value).strip())
    except Exception:
        return default
    return max(minimum, min(maximum, parsed))


def _nonempty(value: Any) -> str:
    return str(value or "").strip()


def _resolve_runtime_config(finish_res: Dict[str, Any]) -> Dict[str, Any]:
    runtime = finish_res.get("runtime")
    if not isinstance(runtime, dict):
        runtime = {}

    alpha_base = _nonempty(runtime.get("alpha_base_url")).rstrip("/")
    if not alpha_base:
        alpha_base = DEFAULT_ALPHA_BASE_URL

    order_signer_url = _nonempty(runtime.get("order_signer_url")) or DEFAULT_ORDER_SIGNER_URL

    market_discovery_url = _nonempty(runtime.get("remote_market_discovery_url"))
    if not market_discovery_url:
        market_discovery_url = f"{alpha_base}/v1/discovery/timeframe"

    evsnipe_discovery_url = _nonempty(runtime.get("remote_evsnipe_discovery_url"))
    if not evsnipe_discovery_url:
        evsnipe_discovery_url = f"{alpha_base}/v1/discovery/evsnipe"
    evcurve_alpha_url = _nonempty(runtime.get("remote_evcurve_alpha_url"))
    if not evcurve_alpha_url:
        evcurve_alpha_url = f"{alpha_base}/v1/alpha/evcurve"
    sessionband_alpha_url = _nonempty(runtime.get("remote_sessionband_alpha_url"))
    if not sessionband_alpha_url:
        sessionband_alpha_url = f"{alpha_base}/v1/alpha/sessionband"
    premarket_alpha_url = _nonempty(runtime.get("remote_premarket_alpha_url"))
    if not premarket_alpha_url:
        premarket_alpha_url = f"{alpha_base}/v1/alpha/premarket/should-trade"
    endgame_alpha_url = _nonempty(runtime.get("remote_endgame_alpha_url"))
    if not endgame_alpha_url:
        endgame_alpha_url = f"{alpha_base}/v1/alpha/endgame/policy"
    mm_rewards_selection_alpha_url = _nonempty(
        runtime.get("remote_mm_rewards_selection_alpha_url")
    )
    if not mm_rewards_selection_alpha_url:
        mm_rewards_selection_alpha_url = f"{alpha_base}/v1/alpha/mm-rewards/selection"
    shared_alpha_token = (
        _nonempty(runtime.get("remote_alpha_token"))
        or _nonempty(runtime.get("remote_discovery_token"))
    )

    return {
        "order_signer_url": order_signer_url,
        "order_signer_token": _nonempty(runtime.get("order_signer_token")),
        "market_discovery_url": market_discovery_url,
        "evsnipe_discovery_url": evsnipe_discovery_url,
        "evcurve_alpha_url": evcurve_alpha_url,
        "sessionband_alpha_url": sessionband_alpha_url,
        "premarket_alpha_url": premarket_alpha_url,
        "endgame_alpha_url": endgame_alpha_url,
        "mm_rewards_selection_alpha_url": mm_rewards_selection_alpha_url,
        "discovery_token": _nonempty(runtime.get("remote_discovery_token")),
        "shared_alpha_token": shared_alpha_token,
        "evsnipe_discovery_timeout_ms": _coerce_int(
            runtime.get("remote_evsnipe_discovery_timeout_ms"), 900, 50, 10_000
        ),
    }


def _resolve_order_signer_primary_token(runtime_cfg: Dict[str, Any], remote_signer_token: str) -> str:
    return _nonempty(runtime_cfg.get("order_signer_token")) or _nonempty(remote_signer_token)


def _post_json(url: str, headers: Dict[str, str], payload: Dict[str, Any]) -> Dict[str, Any]:
    try:
        response = requests.post(url, headers=headers, json=payload, timeout=20)
    except requests.RequestException as exc:
        raise RuntimeError(f"request failed for {url}: {exc}") from exc

    text = response.text.strip()
    try:
        parsed = response.json() if text else {}
    except json.JSONDecodeError:
        parsed = {"raw": text}

    if response.status_code >= 400:
        raise RuntimeError(f"{url} -> {response.status_code}: {parsed}")
    return parsed


def _upsert_env_value(env_path: str, key: str, value: str) -> None:
    path = Path(env_path).expanduser()
    lines = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
    needle = f"{key}="
    replaced = False
    out = []
    for line in lines:
        if line.startswith(needle):
            out.append(f"{key}={value}")
            replaced = True
        else:
            out.append(line)

    if not replaced:
        if out and out[-1].strip():
            out.append("")
        out.append(f"{key}={value}")

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(out) + "\n", encoding="utf-8")


def _read_env_value(env_path: str, key: str) -> str:
    path = Path(env_path).expanduser()
    if not path.exists():
        return ""
    needle = f"{key}="
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith(needle):
            return line.split("=", 1)[1].strip()
    return ""


def _print_base_size_warning(env_file: str) -> None:
    base_size_keys = [
        "EVPOLY_PREMARKET_BASE_SIZE_USD",
        "EVPOLY_ENDGAME_BASE_SIZE_USD",
        "EVPOLY_EVCURVE_BASE_SIZE_USD",
        "EVPOLY_SESSIONBAND_BASE_SIZE_USD",
    ]
    print(
        "IMPORTANT: set strategy base sizes in your env. If left blank, each defaults to 100 USD."
    )
    for key in base_size_keys:
        current = _read_env_value(env_file, key)
        if current:
            print(f"  {key}={current} (configured)")
        else:
            print(f"  {key}= (blank -> default 100 USD)")


def _print_relayer_api_key_notice(env_file: str) -> None:
    relayer_api_key = _read_env_value(env_file, "RELAYER_API_KEY")
    relayer_api_key_address = _read_env_value(env_file, "RELAYER_API_KEY_ADDRESS")
    print(
        "IMPORTANT: redeem/merge uses RELAYER_API_KEY as primary submit path "
        "(remote signer is fallback)."
    )
    print("Get these from: https://polymarket.com/settings?tab=api-keys")
    if relayer_api_key:
        print("  RELAYER_API_KEY=(configured)")
    else:
        print("  RELAYER_API_KEY=(missing)")
    if relayer_api_key_address:
        print(f"  RELAYER_API_KEY_ADDRESS={relayer_api_key_address} (configured)")
    else:
        print("  RELAYER_API_KEY_ADDRESS=(missing)")
    print("Fill them in .env manually, or ask AI to fill them for you.")


def _ensure_env_file(env_path: str, repo_root: Path) -> Tuple[bool, str]:
    path = Path(env_path).expanduser()
    if path.exists():
        return False, ""
    path.parent.mkdir(parents=True, exist_ok=True)
    for seed in (repo_root / ".env.example", repo_root / ".env.full.example"):
        if seed.exists():
            text = seed.read_text(encoding="utf-8")
            if text and not text.endswith("\n"):
                text += "\n"
            path.write_text(text, encoding="utf-8")
            return True, str(seed)
    path.write_text("", encoding="utf-8")
    return True, ""


def _run_post_onboard_approvals(
    command: str,
    private_key: str,
    signature_type: int,
    bind_wallet: str,
    remote_signer_token: str,
) -> None:
    repo_root = Path(__file__).resolve().parents[1]
    cmd = shlex.split(command)
    if not cmd:
        raise RuntimeError("approval bootstrap command is empty")

    env = os.environ.copy()
    env["POLY_PRIVATE_KEY"] = private_key
    env["POLY_SIGNATURE_TYPE"] = str(signature_type)
    if signature_type in (1, 2):
        env["POLY_PROXY_WALLET_ADDRESS"] = bind_wallet
    env["EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN"] = remote_signer_token

    print("approval_bootstrap=start")
    print("approval_bootstrap_cmd=" + " ".join(cmd))
    try:
        subprocess.run(cmd, cwd=repo_root, env=env, check=True)
    except FileNotFoundError as exc:
        raise RuntimeError(
            f"approval bootstrap command not found: {cmd[0]} "
            "(install cargo/rust toolchain, or rerun with --skip-approvals)"
        ) from exc
    except subprocess.CalledProcessError as exc:
        raise RuntimeError(
            f"approval bootstrap failed with exit code {exc.returncode}"
        ) from exc
    print("approval_bootstrap=complete")


def _install_retention_cron(repo_root: Path, cron_expr: str) -> None:
    script = repo_root / "scripts" / "install_evpoly_retention_cron.sh"
    if not script.exists():
        raise RuntimeError(f"retention cron installer missing: {script}")
    env = os.environ.copy()
    if cron_expr.strip():
        env["EVPOLY_RETENTION_CRON_EXPR"] = cron_expr.strip()
    try:
        subprocess.run([str(script)], cwd=repo_root, env=env, check=True)
    except FileNotFoundError as exc:
        raise RuntimeError(
            "retention cron installer failed: required executable not found (bash/crontab)"
        ) from exc
    except subprocess.CalledProcessError as exc:
        raise RuntimeError(
            f"retention cron installer failed with exit code {exc.returncode}"
        ) from exc


def main() -> int:
    parser = argparse.ArgumentParser(description="EVPOLY remote signer self-onboard helper")
    parser.add_argument(
        "--api-base",
        default=DEFAULT_REMOTE_SIGNER_URL,
        help=(
            "API Gateway base URL or /sign URL. "
            "Default: built-in public signer URL."
        ),
    )
    parser.add_argument(
        "--wallet",
        default="",
        help="EOA wallet address used for signature proof. Blank => auto-derived from private key.",
    )
    parser.add_argument(
        "--private-key",
        default=os.getenv("POLY_PRIVATE_KEY", ""),
        help="EOA private key (defaults to POLY_PRIVATE_KEY env)",
    )
    parser.add_argument(
        "--signature-type",
        type=int,
        default=0,
        choices=[0, 1, 2],
        help="0=EOA, 1=Proxy, 2=Safe",
    )
    parser.add_argument(
        "--proxy-wallet",
        default="",
        help="Required for signature_type 1/2; ignored for type 0",
    )
    parser.add_argument(
        "--write-env-file",
        default=".env",
        help=(
            "Write runtime signer/discovery keys to this env file "
            "and auto-generate EVPOLY_ADMIN_API_TOKEN if missing (default: .env)"
        ),
    )
    parser.add_argument(
        "--print-token",
        action="store_true",
        help="Also print token to stdout (off by default to reduce accidental leaks)",
    )
    parser.add_argument(
        "--skip-approvals",
        action="store_true",
        help=(
            "Skip post-onboard approval bootstrap. "
            "By default, signature_type 1/2 runs: cargo run --release --bin test_allowance -- --approve-only"
        ),
    )
    parser.add_argument(
        "--approvals-command",
        default="cargo run --release --bin test_allowance -- --approve-only",
        help="Override approval bootstrap command (executed from repo root).",
    )
    parser.add_argument(
        "--skip-retention-cron",
        action="store_true",
        help=(
            "Skip installing hourly retention cleanup cron. "
            "By default onboarding installs scripts/evpoly_retention_cleanup.sh."
        ),
    )
    parser.add_argument(
        "--retention-cron-expr",
        default="",
        help=(
            "Optional cron expression override for retention cleanup install "
            "(default from env/script: 17 * * * *)."
        ),
    )
    args = parser.parse_args()

    if not args.private_key.strip():
        print("error: private key missing. Set --private-key or POLY_PRIVATE_KEY.", file=sys.stderr)
        return 2

    try:
        derived_eoa = _normalize_wallet(Account.from_key(args.private_key.strip()).address)
    except Exception as exc:
        print(f"error: invalid private key: {exc}", file=sys.stderr)
        return 2

    user_wallet = _normalize_wallet(args.wallet) if args.wallet.strip() else derived_eoa
    if args.wallet.strip() and user_wallet != derived_eoa:
        print("error: --wallet does not match the provided private key.", file=sys.stderr)
        print(f"derived_eoa={derived_eoa}", file=sys.stderr)
        print(f"provided_wallet={user_wallet}", file=sys.stderr)
        print(
            "hint: for signature_type=1, wallet must be the EOA from POLY_PRIVATE_KEY and proxy wallet goes in --proxy-wallet.",
            file=sys.stderr,
        )
        return 2

    bind_wallet = user_wallet
    if args.signature_type in (1, 2):
        if not args.proxy_wallet.strip():
            print(
                "error: --proxy-wallet is required when --signature-type is 1 or 2.",
                file=sys.stderr,
            )
            return 2
        bind_wallet = _normalize_wallet(args.proxy_wallet)
    elif args.proxy_wallet.strip():
        print("warning: --proxy-wallet is ignored when --signature-type is 0.", file=sys.stderr)

    if args.signature_type in (1, 2) and bind_wallet == user_wallet:
        print(
            "warning: proxy wallet equals EOA. This is unusual for signature_type=1/2; double-check your wallet settings.",
            file=sys.stderr,
        )

    print("onboard preflight")
    print(f"signature_type={args.signature_type}")
    print(f"derived_eoa={derived_eoa}")
    print(f"wallet_for_signature={user_wallet}")
    print(f"bind_wallet={bind_wallet}")

    api_base = _normalize_base(args.api_base)
    start_url = f"{api_base}/onboard/start"
    finish_url = f"{api_base}/onboard/finish"
    headers = _headers()

    start_payload: Dict[str, Any] = {
        "wallet": user_wallet,
        "signature_type": args.signature_type,
    }
    if args.signature_type in (1, 2):
        start_payload["proxy_wallet"] = bind_wallet

    start_res = _post_json(start_url, headers, start_payload)
    challenge_id = start_res.get("challenge_id")
    message = start_res.get("message")
    if not challenge_id or not message:
        print(f"error: invalid /onboard/start response: {start_res}", file=sys.stderr)
        return 1

    signed = Account.sign_message(encode_defunct(text=message), private_key=args.private_key.strip())
    signature = signed.signature.hex()

    finish_payload: Dict[str, Any] = {
        "challenge_id": challenge_id,
        "wallet": user_wallet,
        "signature": signature,
        "signature_type": args.signature_type,
    }
    if args.signature_type in (1, 2):
        finish_payload["proxy_wallet"] = bind_wallet

    finish_res = _post_json(finish_url, headers, finish_payload)
    remote_signer_token = str(
        finish_res.get("remote_signer_token")
        or finish_res.get("signer_token")
        or finish_res.get("token")
        or finish_res.get("api_key")
        or ""
    ).strip()
    if not remote_signer_token:
        print(
            f"error: invalid /onboard/finish response (missing remote signer token): {finish_res}",
            file=sys.stderr,
        )
        return 1

    print("onboard complete")
    print(f"wallet_bind={finish_res.get('wallet_address', '')}")
    print(f"allowed_paths={finish_res.get('allowed_paths', [])}")
    repo_root = Path(__file__).resolve().parents[1]

    env_file = str(Path(args.write_env_file).expanduser())
    env_created, env_seed = _ensure_env_file(env_file, repo_root)
    if env_created:
        print(f"env_created={env_file}")
        print(f"env_seed={env_seed if env_seed else '<empty>'}")
    runtime_cfg = _resolve_runtime_config(finish_res)
    wrote_keys = []

    _upsert_env_value(
        env_file, "EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN", remote_signer_token
    )
    wrote_keys.append("EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN")
    _upsert_env_value(
        env_file, "EVPOLY_ORDER_SIGNER_PRIMARY_URL", runtime_cfg["order_signer_url"]
    )
    wrote_keys.append("EVPOLY_ORDER_SIGNER_PRIMARY_URL")
    _upsert_env_value(
        env_file,
        "EVPOLY_REMOTE_MARKET_DISCOVERY_URL",
        runtime_cfg["market_discovery_url"],
    )
    wrote_keys.append("EVPOLY_REMOTE_MARKET_DISCOVERY_URL")
    _upsert_env_value(
        env_file,
        "EVPOLY_REMOTE_EVSNIPE_DISCOVERY_URL",
        runtime_cfg["evsnipe_discovery_url"],
    )
    wrote_keys.append("EVPOLY_REMOTE_EVSNIPE_DISCOVERY_URL")
    _upsert_env_value(
        env_file,
        "EVPOLY_REMOTE_EVCURVE_ALPHA_URL",
        runtime_cfg["evcurve_alpha_url"],
    )
    wrote_keys.append("EVPOLY_REMOTE_EVCURVE_ALPHA_URL")
    _upsert_env_value(
        env_file,
        "EVPOLY_REMOTE_SESSIONBAND_ALPHA_URL",
        runtime_cfg["sessionband_alpha_url"],
    )
    wrote_keys.append("EVPOLY_REMOTE_SESSIONBAND_ALPHA_URL")
    _upsert_env_value(
        env_file,
        "EVPOLY_REMOTE_PREMARKET_ALPHA_URL",
        runtime_cfg["premarket_alpha_url"],
    )
    wrote_keys.append("EVPOLY_REMOTE_PREMARKET_ALPHA_URL")
    _upsert_env_value(
        env_file,
        "EVPOLY_REMOTE_ENDGAME_ALPHA_URL",
        runtime_cfg["endgame_alpha_url"],
    )
    wrote_keys.append("EVPOLY_REMOTE_ENDGAME_ALPHA_URL")
    _upsert_env_value(
        env_file,
        "EVPOLY_REMOTE_MM_REWARDS_SELECTION_ALPHA_URL",
        runtime_cfg["mm_rewards_selection_alpha_url"],
    )
    wrote_keys.append("EVPOLY_REMOTE_MM_REWARDS_SELECTION_ALPHA_URL")
    _upsert_env_value(
        env_file,
        "EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TIMEOUT_MS",
        str(runtime_cfg["evsnipe_discovery_timeout_ms"]),
    )
    wrote_keys.append("EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TIMEOUT_MS")
    if runtime_cfg["discovery_token"]:
        _upsert_env_value(
            env_file,
            "EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN",
            runtime_cfg["discovery_token"],
        )
        _upsert_env_value(
            env_file,
            "EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN",
            runtime_cfg["discovery_token"],
        )
        wrote_keys.append("EVPOLY_REMOTE_MARKET_DISCOVERY_TOKEN")
        wrote_keys.append("EVPOLY_REMOTE_EVSNIPE_DISCOVERY_TOKEN")
    else:
        print(
            "warning: runtime discovery token not provided by onboarding API; "
            "EVPOLY_REMOTE_*_DISCOVERY_TOKEN left unchanged."
        )
    if runtime_cfg["shared_alpha_token"]:
        _upsert_env_value(
            env_file,
            "EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN",
            runtime_cfg["shared_alpha_token"],
        )
        _upsert_env_value(
            env_file,
            "EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN",
            runtime_cfg["shared_alpha_token"],
        )
        _upsert_env_value(
            env_file,
            "EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN",
            runtime_cfg["shared_alpha_token"],
        )
        _upsert_env_value(
            env_file,
            "EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN",
            runtime_cfg["shared_alpha_token"],
        )
        _upsert_env_value(
            env_file,
            "EVPOLY_REMOTE_MM_REWARDS_ALPHA_TOKEN",
            runtime_cfg["shared_alpha_token"],
        )
        wrote_keys.append("EVPOLY_REMOTE_EVCURVE_ALPHA_TOKEN")
        wrote_keys.append("EVPOLY_REMOTE_SESSIONBAND_ALPHA_TOKEN")
        wrote_keys.append("EVPOLY_REMOTE_PREMARKET_ALPHA_TOKEN")
        wrote_keys.append("EVPOLY_REMOTE_ENDGAME_ALPHA_TOKEN")
        wrote_keys.append("EVPOLY_REMOTE_MM_REWARDS_ALPHA_TOKEN")
    else:
        print(
            "warning: runtime alpha token not provided by onboarding API; "
            "EVPOLY_REMOTE_*_ALPHA_TOKEN left unchanged."
        )

    order_signer_primary_token = _resolve_order_signer_primary_token(
        runtime_cfg, remote_signer_token
    )
    _upsert_env_value(
        env_file, "EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN", order_signer_primary_token
    )

    admin_token = _read_env_value(env_file, "EVPOLY_ADMIN_API_TOKEN")
    if not admin_token:
        generated_admin_token = secrets.token_urlsafe(32)
        _upsert_env_value(env_file, "EVPOLY_ADMIN_API_TOKEN", generated_admin_token)
        wrote_keys.append("EVPOLY_ADMIN_API_TOKEN")
        print("admin_api_token=generated")
    else:
        print("admin_api_token=existing")

    print(f"env_updated={env_file}")
    print("wrote_keys=" + ",".join(wrote_keys))
    _print_base_size_warning(env_file)
    _print_relayer_api_key_notice(env_file)
    if args.skip_retention_cron:
        print("retention_cron=skipped")
    else:
        try:
            _install_retention_cron(repo_root, args.retention_cron_expr)
            print("retention_cron=installed")
        except RuntimeError as exc:
            print(f"warning: {exc}", file=sys.stderr)
            print("retention_cron=install_failed")
    if args.signature_type in (1, 2):
        if args.skip_approvals:
            print("approval_bootstrap=skipped")
        else:
            try:
                _run_post_onboard_approvals(
                    args.approvals_command,
                    args.private_key.strip(),
                    args.signature_type,
                    bind_wallet,
                    remote_signer_token,
                )
            except RuntimeError as exc:
                print(f"error: {exc}", file=sys.stderr)
                return 1
    if args.print_token:
        print("remote_signer_token=" + remote_signer_token)

    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(2)
    except RuntimeError as exc:
        print(f"error: {exc}", file=sys.stderr)
        msg = str(exc).lower()
        if "wallet mismatch" in msg:
            print(
                "hint: wallet mismatch usually means token is bound to a different proxy wallet. Re-run onboarding with signature_type=1 and the correct --proxy-wallet.",
                file=sys.stderr,
            )
        raise SystemExit(1)
