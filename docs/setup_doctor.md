# Setup Doctor

Use the Doctor button on Home to check your imported private key, wallet mode and matching funder address. Wallet identity is derived locally; Doctor does not contact EVPOLY onboarding or generate Alpha/signer tokens.

Proxy/Safe gasless wallet operations require user-owned `RELAYER_API_KEY` and `RELAYER_API_KEY_ADDRESS` from [Polymarket API Keys](https://polymarket.com/settings?tab=api-keys). Doctor reports missing relayer credentials as manual items; it does not block CLOB trading on those credentials.

Deposit Wallet profiles keep their existing signer, funder and signature type. Their wallet must already be deployed, funded and approved before trading; this release does not provision deposit wallets or add support for their relayer operations.

Doctor is advisory. It reports `ready`, `fixed` (local wallet fields repaired), `needs_you` (manual fields missing), or `failed` (execution error). Existing profiles do not need an EVPOLY service account or remote credentials.
