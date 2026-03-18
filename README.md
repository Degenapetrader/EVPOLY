# EVPoly Desktop

Cross-platform desktop app for the EVPoly trading bot. Built with Tauri v2 + React.

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

### One-Click Windows Installer

- The installer target is NSIS (`.exe`) and includes WebView2 via `offlineInstaller` mode.
- The app bundles the EVPOLY bot as a Windows sidecar:
  - `src-tauri/binaries/evpoly-bot-x86_64-pc-windows-msvc.exe`
- Release pipeline auto-builds this sidecar from `Degenapetrader/EVPOLY` main during CI.

### Required GitHub Secrets (for updater signing)

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

If signing secrets are missing, release build can fail when generating updater artifacts.
