# EVPoly Desktop

Cross-platform desktop app for the EVPoly trading bot. Built with Tauri v2 + React.

## Official Links

- Website: [https://www.evplus.ai/](https://www.evplus.ai/)
- X: [https://x.com/EVplusAI](https://x.com/EVplusAI)
- GitHub: [https://github.com/Degenapetrader/EVPOLY](https://github.com/Degenapetrader/EVPOLY)

## Restricted Jurisdictions

EVPoly is unavailable in certain restricted jurisdictions due to regulatory, sanctions, or platform restrictions. Use from restricted jurisdictions, or use of VPNs and proxies to bypass geographic restrictions, is prohibited.

- Terms: [TERMS_OF_SERVICE.md](./TERMS_OF_SERVICE.md)
- Policy: [RESTRICTED_JURISDICTIONS.md](./RESTRICTED_JURISDICTIONS.md)

## Prerequisites

- [Rust](https://rustup.rs/) (stable)
- [Node.js](https://nodejs.org/) >= 18
- Linux: `sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf`

## Development

```bash
npm install
npm run tauri:dev
```

## Production Build

```bash
npm run tauri:build
```

Installers are output to `src-tauri/target/release/bundle/`.

## Sidecar Binary

The app bundles the EVPoly bot binary as a sidecar. Place the compiled bot binary at:

```
src-tauri/binaries/evpoly-bot-x86_64-unknown-linux-gnu    # Linux
src-tauri/binaries/evpoly-bot-x86_64-pc-windows-msvc.exe  # Windows
src-tauri/binaries/evpoly-bot-aarch64-apple-darwin         # macOS ARM
src-tauri/binaries/evpoly-bot-x86_64-apple-darwin          # macOS Intel
```

Build it from the repo root:

```bash
cargo build --release --bin polymarket-arbitrage-bot
cp target/release/polymarket-arbitrage-bot src-tauri/binaries/evpoly-bot-$(rustc -vV | grep host | cut -d' ' -f2)
```

## Release

Push to desktop branch, then create a UI tag:

```bash
git push origin desktop
git tag UI-v1.0.0
git push origin UI-v1.0.0
```

The GitHub workflow builds and publishes a **Windows NSIS installer (.exe)** on tag push.

### Fast Release Lane (Recommended for Hotfixes)

Before push/tag, run the fast gate locally:

```bash
INSTALL_DEPS=1 RUN_FRONTEND_BUILD=1 RUN_CARGO_CHECK=1 RUN_SMOKE=1 bash scripts/desktop-gate-fast.sh
```

You can also use hotfix tags:

```bash
git tag UI-hotfix-v1.0.13
git push origin UI-hotfix-v1.0.13
```

Both `UI-v*` and `UI-hotfix-v*` trigger the release workflow. The fast lane now builds EVPOLY sidecars once (in the installer job), instead of rebuilding in every preflight job.

### One-Click Windows Installer

- The installer target is NSIS (`.exe`) and includes WebView2 via `offlineInstaller` mode.
- The app bundles the EVPOLY bot as a Windows sidecar:
  - `src-tauri/binaries/evpoly-bot-x86_64-pc-windows-msvc.exe`
- Release pipeline auto-builds this sidecar from `Degenapetrader/EVPOLY` main during CI.

### Required GitHub Secrets (for updater signing)

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

If signing secrets are missing, release build can fail when generating updater artifacts.
