import { Magic } from "magic-sdk";

let currentKey = "";
let currentMagic: Magic | null = null;

function ensureMagicClient(publishableKey: string): Magic {
  const trimmed = publishableKey.trim();
  if (!trimmed) {
    throw new Error("Magic publishable key is required.");
  }

  if (!currentMagic || currentKey !== trimmed) {
    currentMagic = new Magic(trimmed);
    currentKey = trimmed;
  }
  return currentMagic;
}

export async function createMagicDidToken(
  publishableKey: string,
  email: string
): Promise<string> {
  const normalizedEmail = email.trim();
  if (!normalizedEmail) {
    throw new Error("Email is required.");
  }

  const magic = ensureMagicClient(publishableKey);
  try {
    await magic.user.logout();
  } catch {
    // Best effort only; an existing session should not block a fresh OTP.
  }

  const didToken = await magic.auth.loginWithEmailOTP({
    email: normalizedEmail,
    showUI: true,
  });
  if (!didToken?.trim()) {
    throw new Error("Magic did not return a DID token.");
  }
  return didToken;
}

export async function clearMagicClientSession(): Promise<void> {
  if (!currentMagic) {
    return;
  }
  try {
    await currentMagic.user.logout();
  } catch {
    // Ignore logout cleanup failures.
  }
}
