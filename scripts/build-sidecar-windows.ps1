param(
  [string]$CoreRef,
  [string]$CoreRepo,
  [switch]$Force
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true

$repoRoot = Split-Path -Parent $PSScriptRoot
$lockPath = Join-Path $repoRoot "src-tauri\sidecar-core.lock"
$binariesDir = Join-Path $repoRoot "src-tauri\binaries"
$targetDir = Join-Path $repoRoot "src-tauri\target\core-sidecars"
$coreContractDir = Join-Path $repoRoot "src-tauri\core-contract"
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"

if ((Test-Path $cargoBin) -and -not (($env:Path -split ';') -contains $cargoBin)) {
  $env:Path = "$cargoBin;$env:Path"
}

if (Test-Path $lockPath) {
  foreach ($line in Get-Content $lockPath) {
    if ($line -match '^\s*#') {
      continue
    }
    if ($line -match '^\s*([^=\s]+)\s*=\s*(.+?)\s*$') {
      $key = $matches[1]
      $value = $matches[2]
      if (-not $CoreRef -and $key -eq "CORE_REF") {
        $CoreRef = $value
      }
      if (-not $CoreRepo -and $key -eq "CORE_REPO") {
        $CoreRepo = $value
      }
    }
  }
}

if (-not $CoreRef) {
  $CoreRef = "main"
}
if (-not $CoreRepo) {
  $CoreRepo = "https://github.com/Degenapetrader/EVPOLY.git"
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

$botOutput = Join-Path $binariesDir ("evpoly-bot-" + $targetTriple + ".exe")
$manualOutput = Join-Path $binariesDir ("evpoly-manual-bot-" + $targetTriple + ".exe")
$stampPath = Join-Path $targetDir ("sidecar-build-stamp-" + $targetTriple + ".lock")

if ((-not $Force) -and (Test-Path $botOutput) -and (Test-Path $manualOutput) -and (Test-Path $stampPath)) {
  $stamp = @{}
  foreach ($line in Get-Content $stampPath) {
    if ($line -match '^\s*([^=\s]+)\s*=\s*(.+?)\s*$') {
      $stamp[$matches[1]] = $matches[2]
    }
  }
  if ($stamp["CORE_REF"] -eq $CoreRef -and $stamp["TARGET_TRIPLE"] -eq $targetTriple) {
    Write-Host ("[build-sidecar-windows] sidecars already prepared core_ref={0} target={1}" -f $CoreRef, $targetTriple)
    exit 0
  }
}

$workRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$workDir = Join-Path $workRoot ("evpoly-core-windows-" + [Guid]::NewGuid().ToString("N"))
$useLocalWorktree = $false
$sourceMode = "remote-clone"

function Test-GitCommitExists([string]$Ref) {
  & git rev-parse --verify ("{0}^{{commit}}" -f $Ref) *> $null
  return ($LASTEXITCODE -eq 0)
}

try {
  if (Test-GitCommitExists $CoreRef) {
    Write-Host ("[build-sidecar-windows] creating local worktree ref={0}" -f $CoreRef)
    git worktree add --detach $workDir $CoreRef
    if ($LASTEXITCODE -ne 0) {
      throw "git worktree add failed"
    }
    $useLocalWorktree = $true
    $sourceMode = "local-worktree"
  } else {
    Write-Host ("[build-sidecar-windows] cloning {0} ref={1}" -f $CoreRepo, $CoreRef)
    git clone --filter=blob:none $CoreRepo $workDir
    if ($LASTEXITCODE -ne 0) {
      throw "git clone failed"
    }
    git -C $workDir checkout --detach $CoreRef
    if ($LASTEXITCODE -ne 0) {
      throw "git checkout failed"
    }
  }

  $manifest = Join-Path $workDir "Cargo.toml"
  Write-Host ("[build-sidecar-windows] building sidecar binaries ref={0} target={1}" -f $CoreRef, $targetTriple)
  cargo build --release --manifest-path $manifest --target-dir $targetDir --target $targetTriple --bin polymarket-arbitrage-bot --bin manual_bot
  if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed"
  }

  New-Item -ItemType Directory -Force -Path $binariesDir | Out-Null
  New-Item -ItemType Directory -Force -Path $targetDir | Out-Null
  New-Item -ItemType Directory -Force -Path $coreContractDir | Out-Null

  $releaseDir = Join-Path (Join-Path $targetDir $targetTriple) "release"
  Copy-Item (Join-Path $releaseDir "polymarket-arbitrage-bot.exe") $botOutput -Force
  Copy-Item (Join-Path $releaseDir "manual_bot.exe") $manualOutput -Force
  $coreEnvPath = Join-Path $coreContractDir ".env.example"
  Copy-Item (Join-Path $workDir ".env.example") $coreEnvPath -Force
  $coreEnvContent = Get-Content -Path $coreEnvPath -Raw
  if ($coreEnvContent -notmatch '(?m)^EVPOLY_MM_MARKET_MODE=') {
    Add-Content -Path $coreEnvPath -Value @"

# MM rewards market selection mode (`auto` = local rewards discovery only;
# `hybrid` also honors single-market selectors when present)
EVPOLY_MM_MARKET_MODE=auto
"@
  }

  @(
    "CORE_REF=$CoreRef",
    "CORE_REPO=$CoreRepo",
    "TARGET_TRIPLE=$targetTriple",
    "SOURCE_MODE=$sourceMode",
    "PREPARED_AT_UTC=$([DateTime]::UtcNow.ToString('o'))"
  ) | Set-Content -Path $stampPath -Encoding UTF8

  Write-Host "[build-sidecar-windows] done"
}
finally {
  if (Test-Path $workDir) {
    if ($useLocalWorktree) {
      git worktree remove --force $workDir
    } else {
      Remove-Item -Path $workDir -Recurse -Force
    }
  }
}
