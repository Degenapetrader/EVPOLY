param(
  [switch]$Force
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true

$repoRoot = Split-Path -Parent $PSScriptRoot
$coreDir = Join-Path $repoRoot "core"
$binariesDir = Join-Path $repoRoot "src-tauri\binaries"
$targetDir = Join-Path $repoRoot "src-tauri\target\core-sidecars"
$coreContractDir = Join-Path $repoRoot "src-tauri\core-contract"
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
$manifest = Join-Path $coreDir "Cargo.toml"
$buildDrive = $null
$createdBuildDrive = $false
$buildRoot = $repoRoot
$buildCoreDir = $coreDir
$buildTargetDir = $targetDir

if ((Test-Path $cargoBin) -and -not (($env:Path -split ';') -contains $cargoBin)) {
  $env:Path = "$cargoBin;$env:Path"
}

# This Windows machine has been crashing inside rustc during optimized
# release builds. Keep sidecar release builds on the same conservative
# local profile used by the desktop shell build path.
if (-not $env:CARGO_BUILD_JOBS) {
  $env:CARGO_BUILD_JOBS = "1"
}
if (-not $env:CARGO_INCREMENTAL) {
  $env:CARGO_INCREMENTAL = "0"
}
if (-not $env:CARGO_PROFILE_DEV_DEBUG) {
  $env:CARGO_PROFILE_DEV_DEBUG = "0"
}
if (-not $env:CARGO_PROFILE_DEV_CODEGEN_UNITS) {
  $env:CARGO_PROFILE_DEV_CODEGEN_UNITS = "1"
}
if (-not $env:CARGO_PROFILE_RELEASE_OPT_LEVEL) {
  $env:CARGO_PROFILE_RELEASE_OPT_LEVEL = "0"
}
if (-not $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS) {
  $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1"
}

if (-not (Test-Path $manifest)) {
  throw "Desktop-owned core source is missing: $manifest"
}

function Get-SubstTarget([string]$driveLetter) {
  $drive = $driveLetter.TrimEnd(':') + ':'
  $output = cmd /c "subst $drive" 2>$null
  if ($LASTEXITCODE -ne 0 -or -not $output) {
    return $null
  }
  $line = ($output | Select-Object -First 1)
  if ($line -match '=>\s*(.+)$') {
    return $matches[1].Trim()
  }
  return $null
}

function Mount-ShortRepoDrive([string]$repoPath) {
  foreach ($candidate in @('R:', 'S:', 'T:', 'U:')) {
    $existing = Get-SubstTarget $candidate
    if ($existing) {
      if ([IO.Path]::GetFullPath($existing) -eq [IO.Path]::GetFullPath($repoPath)) {
        return @{ Drive = $candidate; Created = $false }
      }
      continue
    }
    cmd /c "subst $candidate `"$repoPath`"" | Out-Null
    if ($LASTEXITCODE -eq 0) {
      return @{ Drive = $candidate; Created = $true }
    }
  }
  return $null
}

$targetTriple = $null
foreach ($line in (& rustc -vV)) {
  if ($line.StartsWith("host: ")) {
    $targetTriple = $line.Substring(6).Trim()
    break
  }
}
if (-not $targetTriple) {
  throw "Unable to determine Rust host target triple"
}

$driveMount = Mount-ShortRepoDrive $repoRoot
if ($driveMount) {
  $buildDrive = $driveMount.Drive
  $createdBuildDrive = [bool]$driveMount.Created
  $buildRoot = "$buildDrive\"
  $buildCoreDir = Join-Path $buildRoot "core"
  $buildTargetDir = Join-Path $buildRoot "t-sidecars"
  $manifest = Join-Path $buildCoreDir "Cargo.toml"
}

$botOutput = Join-Path $binariesDir ("evpoly-bot-" + $targetTriple + ".exe")
$manualOutput = Join-Path $binariesDir ("evpoly-manual-bot-" + $targetTriple + ".exe")
$stampPath = Join-Path $targetDir ("sidecar-build-stamp-" + $targetTriple + ".lock")

function Get-CoreLatestWriteTicks {
  $files = Get-ChildItem -Path $coreDir -Recurse -File | Where-Object {
    $_.FullName -notmatch '\\target\\' -and $_.FullName -notmatch '\\.git\\'
  }
  if (-not $files) {
    return 0L
  }
  return ($files | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1).LastWriteTimeUtc.Ticks
}

$coreLatestWriteTicks = Get-CoreLatestWriteTicks

if ((-not $Force) -and (Test-Path $botOutput) -and (Test-Path $manualOutput) -and (Test-Path $stampPath)) {
  $stamp = @{}
  foreach ($line in Get-Content $stampPath) {
    if ($line -match '^\s*([^=\s]+)\s*=\s*(.+?)\s*$') {
      $stamp[$matches[1]] = $matches[2]
    }
  }
  $stampTicks = (Get-Item $stampPath).LastWriteTimeUtc.Ticks
  if (
    $stamp["CORE_SOURCE"] -eq "core" -and
    $stamp["TARGET_TRIPLE"] -eq $targetTriple -and
    $stampTicks -ge $coreLatestWriteTicks
  ) {
    Write-Host ("[build-sidecar-windows] sidecars already prepared source=core target={0}" -f $targetTriple)
    exit 0
  }
}
$buildProfile = "release"

try {
  Write-Host ("[build-sidecar-windows] building sidecar binaries source=core target={0}" -f $targetTriple)
  cargo build --release --manifest-path $manifest --target-dir $buildTargetDir --target $targetTriple --bin polymarket-arbitrage-bot --bin manual_bot
  if ($LASTEXITCODE -ne 0) {
    Write-Warning ("[build-sidecar-windows] release build failed exit_code={0}; retrying debug fallback" -f $LASTEXITCODE)
    $env:CARGO_BUILD_JOBS = "1"
    cargo build --manifest-path $manifest --target-dir $buildTargetDir --target $targetTriple --bin polymarket-arbitrage-bot --bin manual_bot
    if ($LASTEXITCODE -ne 0) {
      throw "cargo build failed"
    }
    $buildProfile = "debug"
  }

  New-Item -ItemType Directory -Force -Path $binariesDir | Out-Null
  New-Item -ItemType Directory -Force -Path $targetDir | Out-Null
  New-Item -ItemType Directory -Force -Path $coreContractDir | Out-Null

  $profileDir = Join-Path (Join-Path $buildTargetDir $targetTriple) $buildProfile
  Copy-Item (Join-Path $profileDir "polymarket-arbitrage-bot.exe") $botOutput -Force
  Copy-Item (Join-Path $profileDir "manual_bot.exe") $manualOutput -Force
  # Desktop owns its runtime env template in this branch. We sync the core bot
  # code from the pinned ref, but keep desktop defaults versioned locally.

  @(
    "CORE_SOURCE=core",
    "TARGET_TRIPLE=$targetTriple",
    "SOURCE_MODE=desktop-local",
    "BUILD_PROFILE=$buildProfile",
    "PREPARED_AT_UTC=$([DateTime]::UtcNow.ToString('o'))"
  ) | Set-Content -Path $stampPath -Encoding UTF8

  Write-Host "[build-sidecar-windows] done"
}
finally {
  if ($createdBuildDrive -and $buildDrive) {
    cmd /c "subst $buildDrive /d" | Out-Null
  }
}
