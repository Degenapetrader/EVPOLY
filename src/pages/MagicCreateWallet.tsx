import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { AppShell } from "../components/AppShell";
import { SectionPanel } from "../components/SectionPanel";
import { useAppContext } from "../App";
import { mergeConfig } from "../lib/desktop-config";
import { completeDesktopMagicWalletOnboarding } from "../lib/desktop-magic-onboarding";
import {
  deriveWalletAddress,
  getSavedConfig,
  saveConfig,
} from "../lib/tauri-commands";

function getErrorText(err: unknown, fallback: string): string {
  if (typeof err === "string") {
    return err;
  }
  if (err instanceof Error) {
    return err.message;
  }
  return fallback;
}

function isSafeReady(value: string): boolean {
  const normalized = value.trim().toLowerCase();
  return ["ready", "deployed", "active", "ok"].includes(normalized);
}

export function MagicCreateWallet() {
  const navigate = useNavigate();
  const { activeProfileId } = useAppContext();
  const [email, setEmail] = useState("");
  const [loading, setLoading] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const handleCreateWallet = async () => {
    if (!activeProfileId) {
      setMessage("Create or activate a profile first.");
      return;
    }
    if (!email.trim()) {
      setMessage("Enter an email address.");
      return;
    }

    setLoading(true);
    setMessage(null);
    try {
      const result = await completeDesktopMagicWalletOnboarding(email, activeProfileId);
      if (!isSafeReady(result.safeStatus) || !result.safeAddress) {
        setMessage("Wallet created, but the Polymarket safe is not ready yet.");
        return;
      }

      const derivedSigner = await deriveWalletAddress(result.privateKey);
      if (
        result.signerAddress &&
        derivedSigner.trim().toLowerCase() !== result.signerAddress.trim().toLowerCase()
      ) {
        throw new Error("Exported Magic private key does not match the provisioned signer.");
      }

      const saved = mergeConfig(await getSavedConfig(activeProfileId));
      const nextConfig = {
        ...saved,
        private_key: result.privateKey,
        eoa_wallet: derivedSigner,
        sig_type: result.signatureType,
        proxy_wallet: result.safeAddress,
        alpha_key: "",
        relayer_remote_signer_token: "",
        relayer_submit_signer_url: "",
        wallet_binding: "",
        onboarding_status: "wallet_saved",
        approval_status: "",
        remote_signer_token: "",
        order_signer_primary_token_internal: "",
      };
      await saveConfig(activeProfileId, nextConfig);
      setMessage("Wallet saved. Onboarding credentials are ready.");
      navigate("/settings");
    } catch (err) {
      setMessage(getErrorText(err, "failed to create Magic wallet"));
    } finally {
      setLoading(false);
    }
  };

  return (
    <AppShell
      railSubtitle="BY EVPLUS"
      railLogoSrc="/logo.png"
      railLogoAlt="EVPlus"
      railItems={[
        { label: "Home", to: "/home" },
        { label: "Settings", to: "/settings" },
      ]}
      eyebrow="Settings"
      title="Create Wallet"
      description="Create a local signing wallet with Magic email OTP."
      meta={
        <button type="button" onClick={() => navigate("/settings")} className="ui-button">
          Back
        </button>
      }
      banner={message ? <div className="inline-alert inline-alert--warning">{message}</div> : null}
      contentClassName="page-stack"
    >
      <SectionPanel
        title="Email Wallet"
        subtitle="The private key is exported locally and saved into this desktop profile."
      >
        <div className="max-w-xl space-y-4">
          <div>
            <label className="field-label">Email</label>
            <input
              type="email"
              value={email}
              onChange={(event) => setEmail(event.target.value)}
              className="field-input"
              autoComplete="email"
            />
          </div>
          <button
            type="button"
            onClick={() => void handleCreateWallet()}
            disabled={loading}
            className="ui-button ui-button--primary"
          >
            {loading ? "Creating..." : "Create Wallet"}
          </button>
        </div>
      </SectionPanel>
    </AppShell>
  );
}
