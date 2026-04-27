#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
POLICY_FILE="${ROOT_DIR}/.cargo/audit.toml"
TODAY_UTC="$(date -u +%F)"

if [[ ! -f "${POLICY_FILE}" ]]; then
  echo "[security_audit] missing policy file: ${POLICY_FILE}" >&2
  exit 1
fi

expired_count=0
while IFS='|' read -r advisory_id expires_on; do
  [[ -z "${advisory_id}" || -z "${expires_on}" ]] && continue
  if [[ "${TODAY_UTC}" > "${expires_on}" ]]; then
    echo "[security_audit] expired advisory exception: ${advisory_id} (expired ${expires_on})" >&2
    expired_count=$((expired_count + 1))
  fi
done < <(
  awk '
    /^\s*#\s*expires:/ && /id:/ {
      expires = ""
      advisory = ""
      for (i = 1; i <= NF; i++) {
        if ($i == "expires:") expires = $(i + 1)
        if ($i == "id:") advisory = $(i + 1)
      }
      gsub(/[^0-9-]/, "", expires)
      gsub(/[^A-Z0-9-]/, "", advisory)
      if (advisory != "" && expires != "") {
        print advisory "|" expires
      }
    }
  ' "${POLICY_FILE}"
)

if [[ "${expired_count}" -gt 0 ]]; then
  echo "[security_audit] ${expired_count} exception(s) expired; update ${POLICY_FILE} before running audit." >&2
  exit 1
fi

cd "${ROOT_DIR}"
cargo metadata --locked --format-version 1 >/dev/null
cargo audit "$@"
