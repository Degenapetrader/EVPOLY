param(
  [string]$CoreRef = "main",
  [string]$CoreRepo = "https://github.com/Degenapetrader/EVPOLY.git"
)

$ErrorActionPreference = "Stop"

$workRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$workDir = Join-Path $workRoot ("evpoly-core-windows-" + [Guid]::NewGuid().ToString("N"))

try {
  Write-Host ("[build-sidecar-windows] cloning {0} ref={1}" -f $CoreRepo, $CoreRef)
  git clone --depth 1 --branch $CoreRef $CoreRepo $workDir

  $manifest = Join-Path $workDir "Cargo.toml"
  Write-Host "[build-sidecar-windows] building sidecar binaries"
  cargo build --release --manifest-path $manifest --bin polymarket-arbitrage-bot
  cargo build --release --manifest-path $manifest --bin manual_bot

  New-Item -ItemType Directory -Force -Path "src-tauri\binaries" | Out-Null
  Copy-Item (Join-Path $workDir "target\release\polymarket-arbitrage-bot.exe") "src-tauri\binaries\evpoly-bot-x86_64-pc-windows-msvc.exe" -Force
  Copy-Item (Join-Path $workDir "target\release\manual_bot.exe") "src-tauri\binaries\evpoly-manual-bot-x86_64-pc-windows-msvc.exe" -Force
  Write-Host "[build-sidecar-windows] done"
}
finally {
  if (Test-Path $workDir) {
    Remove-Item -Path $workDir -Recurse -Force
  }
}
