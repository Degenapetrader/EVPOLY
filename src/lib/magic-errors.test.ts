import { describe, expect, it } from "vitest";

import { humanizeMagicProvisioningError } from "./magic-errors";

describe("humanizeMagicProvisioningError", () => {
  it("humanizes raw Magic Core provisioning codes", () => {
    expect(
      humanizeMagicProvisioningError(
        "magic_core_wallet_create_incomplete",
        "failed"
      )
    ).toBe("Wallet setup failed. Retry wallet setup or contact support.");
  });

  it("humanizes Tauri bridge errors that include a JSON body", () => {
    expect(
      humanizeMagicProvisioningError(
        new Error(
          'Magic bridge returned 400: {"ok":false,"error":"magic_core_wallet_group_missing"}'
        ),
        "failed"
      )
    ).toBe("Wallet setup failed. Retry wallet setup or contact support.");
  });

  it("keeps unrelated errors visible", () => {
    expect(
      humanizeMagicProvisioningError(new Error("OTP login failed"), "failed")
    ).toBe("OTP login failed");
  });

  it("humanizes Magic Core export and secret errors", () => {
    expect(
      humanizeMagicProvisioningError(
        "magic_core_wallet_secrets_missing",
        "failed"
      )
    ).toBe("Wallet setup failed. Retry wallet setup or contact support.");
    expect(
      humanizeMagicProvisioningError(
        "magic_core_reveal_private_key_unencrypted_response",
        "failed"
      )
    ).toBe("Wallet setup failed. Retry wallet setup or contact support.");
  });

  it("falls back when the thrown value has no message", () => {
    expect(humanizeMagicProvisioningError({ ok: false }, "failed")).toBe(
      "failed"
    );
  });
});
