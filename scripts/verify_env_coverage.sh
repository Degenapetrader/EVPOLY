#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

ENV_FILE=""
FIX_MISSING=false
STRICT_MISSING=false

usage() {
  cat <<'USAGE'
Usage: scripts/verify_env_coverage.sh [--env-file FILE] [--fix] [--strict]

Checks whether an env file contains all runtime env keys referenced by code.

Options:
  --env-file FILE   Env file to verify (default: .env)
  --fix             Append missing keys as blank placeholders (KEY=)
  --strict          Exit non-zero when missing keys are found
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --env-file)
      shift
      [[ $# -gt 0 ]] || { echo "Missing value for --env-file" >&2; exit 2; }
      ENV_FILE="$1"
      ;;
    --fix)
      FIX_MISSING=true
      ;;
    --strict)
      STRICT_MISSING=true
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if [[ -z "$ENV_FILE" ]]; then
  if [[ -f ".env" ]]; then
    ENV_FILE=".env"
  else
    echo "No env file found. Create .env, or pass --env-file FILE." >&2
    exit 2
  fi
fi

if [[ ! -f "$ENV_FILE" ]]; then
  echo "Env file not found: $ENV_FILE" >&2
  exit 2
fi

RUNTIME_KEYS_FILE="$(mktemp)"
ENV_KEYS_FILE="$(mktemp)"
MISSING_KEYS_FILE="$(mktemp)"
UNUSED_KEYS_FILE="$(mktemp)"
trap 'rm -f "$RUNTIME_KEYS_FILE" "$ENV_KEYS_FILE" "$MISSING_KEYS_FILE" "$UNUSED_KEYS_FILE"' EXIT

# Runtime env keys read from code paths.
{
  rg -o --no-filename --glob '!src/bin/**' 'std::env::var\("[A-Z0-9_]+"\)' src
  rg -o --no-filename --glob '!src/bin/**' 'env::var\("[A-Z0-9_]+"\)' src
  # Catch helper wrappers like env_bool(...), env_u64(...), env_nonempty_named(...), etc.
  rg -o --no-filename --glob '!src/bin/**' '(Self::)?env_[a-z0-9_]+\("[A-Z0-9_]+"' src
} | sed -E 's/.*\("([A-Z0-9_]+)".*/\1/' | sort -u > "$RUNTIME_KEYS_FILE"

# Keys present in env file.
awk -F= '/^[[:space:]]*#?[[:space:]]*[A-Za-z_][A-Za-z0-9_]*=/{
  key=$1
  gsub(/^[[:space:]]*#[[:space:]]*/, "", key)
  gsub(/^[[:space:]]+|[[:space:]]+$/, "", key)
  print key
}' "$ENV_FILE" | sort -u > "$ENV_KEYS_FILE"

comm -23 "$RUNTIME_KEYS_FILE" "$ENV_KEYS_FILE" > "$MISSING_KEYS_FILE"
comm -13 "$RUNTIME_KEYS_FILE" "$ENV_KEYS_FILE" > "$UNUSED_KEYS_FILE"

runtime_count=$(wc -l < "$RUNTIME_KEYS_FILE" | tr -d ' ')
env_count=$(wc -l < "$ENV_KEYS_FILE" | tr -d ' ')
missing_count=$(wc -l < "$MISSING_KEYS_FILE" | tr -d ' ')
unused_count=$(wc -l < "$UNUSED_KEYS_FILE" | tr -d ' ')

echo "Env coverage report"
echo "- Env file: $ENV_FILE"
echo "- Runtime keys: $runtime_count"
echo "- Env keys: $env_count"
echo "- Missing keys: $missing_count"
echo "- Unused keys: $unused_count"

if [[ "$missing_count" -gt 0 ]]; then
  echo
  echo "Missing keys:"
  cat "$MISSING_KEYS_FILE"

  if [[ "$FIX_MISSING" == true ]]; then
    {
      echo
      echo "# Added by scripts/verify_env_coverage.sh --fix on $(date -u +%Y-%m-%dT%H:%M:%SZ)"
      while IFS= read -r key; do
        [[ -n "$key" ]] && echo "# ${key}=<set-value>"
      done < "$MISSING_KEYS_FILE"
    } >> "$ENV_FILE"

    echo
    echo "Appended $missing_count commented placeholders to $ENV_FILE"
    exit 0
  fi

  if [[ "$STRICT_MISSING" == true ]]; then
    echo
    echo "Strict mode enabled: missing keys fail verification."
    echo "Run with --fix to append placeholders automatically."
    exit 1
  fi

  echo
  echo "Missing keys are informational in non-strict mode."
  echo "Use --strict to fail on missing keys."
fi

echo
if [[ "$unused_count" -gt 0 ]]; then
  if [[ "$missing_count" -gt 0 ]]; then
    echo "Unused env keys present (informational):"
  else
    echo "No missing keys. Unused env keys present (informational):"
  fi
  cat "$UNUSED_KEYS_FILE"
else
  if [[ "$missing_count" -gt 0 ]]; then
    echo "No unused env keys."
  else
    echo "No missing keys. Coverage is complete."
  fi
fi
