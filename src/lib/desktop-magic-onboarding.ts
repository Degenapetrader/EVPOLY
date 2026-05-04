import { createMagicDidToken, clearMagicClientSession } from "./magic-client";
import {
  desktopMagicFinish,
  desktopMagicStart,
  type DesktopMagicFinishResult,
  type DesktopMagicStartResult,
} from "./tauri-commands";

const RSA_EXPORT_ALGORITHM = "RSA-OAEP";

export interface DesktopMagicWalletResult {
  privateKey: string;
  signerAddress: string;
  signatureType: number;
  depositWalletAddress: string;
  activeWalletAddress: string;
  provisioningStatus: string;
}

function assertSubtleCrypto(): SubtleCrypto {
  if (typeof window === "undefined" || !window.crypto?.subtle) {
    throw new Error("Secure wallet export requires a modern desktop webview.");
  }
  return window.crypto.subtle;
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let index = 0; index < bytes.length; index += 1) {
    binary += String.fromCharCode(bytes[index]);
  }
  return window.btoa(binary);
}

function base64ToBytes(value: string): Uint8Array {
  const binary = window.atob(value.trim());
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

function bytesToArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

function startSessionId(result: DesktopMagicStartResult): string {
  return String(
    result.desktop_onboard_session_id ??
      result.session_id ??
      result.onboard_session_id ??
      ""
  ).trim();
}

function publishableKey(result: DesktopMagicStartResult): string {
  return String(
    result.publishable_key ??
      result.magic_publishable_key ??
      result.magic?.publishable_key ??
      ""
  ).trim();
}

function encryptedPrivateKey(result: DesktopMagicFinishResult): string {
  return String(
    result.encrypted_private_key ??
      result.encryptedPrivateKey ??
      ""
  ).trim();
}

async function generateExportKeyPair(): Promise<{
  decryptKey: CryptoKey;
  publicKeyPem: string;
}> {
  const subtle = assertSubtleCrypto();
  const pair = await subtle.generateKey(
    {
      name: RSA_EXPORT_ALGORITHM,
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-1",
    },
    true,
    ["encrypt", "decrypt"]
  );
  const exportedPublicKey = await subtle.exportKey("spki", pair.publicKey);
  const publicKeyBase64 = bytesToBase64(new Uint8Array(exportedPublicKey));
  const publicKeyPem = `-----BEGIN PUBLIC KEY-----\n${
    publicKeyBase64.match(/.{1,64}/g)?.join("\n") ?? publicKeyBase64
  }\n-----END PUBLIC KEY-----`;
  return { decryptKey: pair.privateKey, publicKeyPem };
}

async function decryptExportedPrivateKey(
  encryptedValue: string,
  decryptKey: CryptoKey
): Promise<string> {
  const decrypted = await assertSubtleCrypto().decrypt(
    { name: RSA_EXPORT_ALGORITHM },
    decryptKey,
    bytesToArrayBuffer(base64ToBytes(encryptedValue))
  );
  const privateKey = new TextDecoder().decode(decrypted).trim();
  if (!privateKey) {
    throw new Error("Magic export returned an empty private key.");
  }
  return privateKey;
}

export async function completeDesktopMagicWalletOnboarding(
  email: string,
  profileId: string | null
): Promise<DesktopMagicWalletResult> {
  const start = await desktopMagicStart(email, profileId);
  const sessionId = startSessionId(start);
  const key = publishableKey(start);
  if (!sessionId) {
    throw new Error("Magic bridge did not return an onboarding session.");
  }
  if (!key) {
    throw new Error("Magic bridge did not return a publishable key.");
  }

  const didToken = await createMagicDidToken(key, email);
  try {
    const { decryptKey, publicKeyPem } = await generateExportKeyPair();
    const finish = await desktopMagicFinish(sessionId, didToken, publicKeyPem);
    const encrypted = encryptedPrivateKey(finish);
    if (!encrypted) {
      throw new Error("Magic bridge did not return an encrypted private key.");
    }

    const privateKey = await decryptExportedPrivateKey(encrypted, decryptKey);
    const signerAddress = String(finish.signer_address ?? "").trim();
    const signatureType = Number(
      finish.signature_type ?? (finish.deposit_wallet_address ? 3 : 2)
    );
    const activeWalletAddress = String(finish.active_wallet_address ?? "").trim();
    const depositWalletAddress = String(
      finish.deposit_wallet_address ??
        (signatureType === 3 ? activeWalletAddress : "") ??
        ""
    ).trim();
    const provisioningStatus = String(finish.provisioning_status ?? "").trim();

    return {
      privateKey,
      signerAddress,
      signatureType,
      depositWalletAddress,
      activeWalletAddress,
      provisioningStatus,
    };
  } finally {
    await clearMagicClientSession();
  }
}
