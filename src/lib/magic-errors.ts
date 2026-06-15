const MAGIC_WALLET_SETUP_FAILED =
  "Wallet setup failed. Retry wallet setup or contact support.";

const MAGIC_CORE_PROVISIONING_ERRORS = [
  "magic_core_wallet_create_incomplete",
  "magic_core_request_failed",
  "magic_core_not_configured",
  "magic_core_wallet_group_missing",
  "magic_core_wallet_recovery_key_missing",
  "magic_core_wallet_recovery_incomplete",
  "magic_core_wallet_secrets_missing",
  "magic_core_reveal_private_key_missing",
  "magic_core_reveal_private_key_unencrypted_response",
] as const;

function readErrorMessage(err: unknown): string {
  if (typeof err === "string") return err.trim();
  if (err instanceof Error) return err.message.trim();
  return "";
}

function readJsonErrorCode(message: string): string | null {
  const jsonStart = message.indexOf("{");
  if (jsonStart < 0) return null;
  try {
    const parsed = JSON.parse(message.slice(jsonStart)) as { error?: unknown };
    return typeof parsed.error === "string" ? parsed.error.trim() : null;
  } catch {
    return null;
  }
}

export function humanizeMagicProvisioningError(
  err: unknown,
  fallback: string
): string {
  const message = readErrorMessage(err);
  const code = readJsonErrorCode(message) ?? message;
  if (MAGIC_CORE_PROVISIONING_ERRORS.some((known) => code.includes(known))) {
    return MAGIC_WALLET_SETUP_FAILED;
  }
  return message || fallback;
}
