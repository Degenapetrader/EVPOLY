param(
  [switch]$InstallDeps,
  [switch]$RunSmoke,
  [bool]$RunFrontendBuild = $true,
  [bool]$RunCargoCheck = $true
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true

$shouldInstallDeps = $InstallDeps -or -not (Test-Path "node_modules")
Write-Host ("[desktop-gate-fast] install_deps={0} frontend_build={1} cargo_check={2} smoke={3}" -f $shouldInstallDeps, $RunFrontendBuild, $RunCargoCheck, $RunSmoke)

if ($shouldInstallDeps) {
  Write-Host "[desktop-gate-fast] npm ci"
  npm ci
}

Write-Host "[desktop-gate-fast] npm test"
npm test

if ($RunFrontendBuild) {
  Write-Host "[desktop-gate-fast] npm run build"
  npm run build
}

if ($RunCargoCheck) {
  Write-Host "[desktop-gate-fast] cargo check"
  cargo check --manifest-path src-tauri/Cargo.toml
}

if ($RunSmoke) {
  Write-Host "[desktop-gate-fast] smoke-release.ps1"
  ./scripts/smoke-release.ps1
}

Write-Host "[desktop-gate-fast] all checks passed"
