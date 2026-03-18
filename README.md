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

Push a version tag to the `desktop` branch:

```bash
git tag v0.1.0
git push origin desktop --tags
```

GitHub Actions builds installers for Windows, macOS, and Linux, and publishes them as a GitHub Release with SHA256 checksums.
