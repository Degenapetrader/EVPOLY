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
$corePatchDir = Join-Path $repoRoot "src-tauri\core-patches"
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"

function Get-CorePatchHash {
  if (-not (Test-Path $corePatchDir)) {
    return "none"
  }

  $patches = Get-ChildItem -Path $corePatchDir -File -Filter *.patch | Sort-Object Name
  if (-not $patches) {
    return "none"
  }

  $sha = [System.Security.Cryptography.SHA256]::Create()
  try {
    foreach ($patch in $patches) {
      $nameBytes = [System.Text.Encoding]::UTF8.GetBytes($patch.Name)
      [void]$sha.TransformBlock($nameBytes, 0, $nameBytes.Length, $nameBytes, 0)
      $contentBytes = [System.IO.File]::ReadAllBytes($patch.FullName)
      [void]$sha.TransformBlock($contentBytes, 0, $contentBytes.Length, $contentBytes, 0)
    }
    [void]$sha.TransformFinalBlock([byte[]]::new(0), 0, 0)
    return ([System.BitConverter]::ToString($sha.Hash)).Replace("-", "").ToLowerInvariant()
  }
  finally {
    $sha.Dispose()
  }
}

$patchHash = Get-CorePatchHash

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
if (-not $env:CARGO_PROFILE_RELEASE_OPT_LEVEL) {
  $env:CARGO_PROFILE_RELEASE_OPT_LEVEL = "0"
}
if (-not $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS) {
  $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1"
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
$stampPath = Join-Path $targetDir ("sidecar-build-stamp-" + $targetTriple + ".lock")

if ((-not $Force) -and (Test-Path $botOutput) -and (Test-Path $stampPath)) {
  $stamp = @{}
  foreach ($line in Get-Content $stampPath) {
    if ($line -match '^\s*([^=\s]+)\s*=\s*(.+?)\s*$') {
      $stamp[$matches[1]] = $matches[2]
    }
  }
  if ($stamp["CORE_REF"] -eq $CoreRef -and $stamp["TARGET_TRIPLE"] -eq $targetTriple -and $stamp["PATCHES_SHA256"] -eq $patchHash) {
    Write-Host ("[build-sidecar-windows] sidecars already prepared core_ref={0} target={1}" -f $CoreRef, $targetTriple)
    exit 0
  }
}

$workRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$workDir = Join-Path $workRoot ("evpoly-core-windows-" + [Guid]::NewGuid().ToString("N"))
$useLocalWorktree = $false
$sourceMode = "remote-clone"
$buildProfile = "release"

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

  if (Test-Path $corePatchDir) {
    $patches = Get-ChildItem -Path $corePatchDir -File -Filter *.patch | Sort-Object Name
    foreach ($patch in $patches) {
      Write-Host ("[build-sidecar-windows] applying core patch {0}" -f $patch.Name)
      git -C $workDir apply --whitespace=nowarn $patch.FullName
      if ($LASTEXITCODE -ne 0) {
        throw ("git apply failed for {0}" -f $patch.Name)
      }
    }
  }

  $manifest = Join-Path $workDir "Cargo.toml"
  Write-Host ("[build-sidecar-windows] building sidecar binaries ref={0} target={1}" -f $CoreRef, $targetTriple)
  cargo build --release --manifest-path $manifest --target-dir $targetDir --target $targetTriple --bin polymarket-arbitrage-bot
  if ($LASTEXITCODE -ne 0) {
    Write-Warning ("[build-sidecar-windows] release build failed exit_code={0}; retrying debug fallback" -f $LASTEXITCODE)
    $env:CARGO_BUILD_JOBS = "1"
    cargo build --manifest-path $manifest --target-dir $targetDir --target $targetTriple --bin polymarket-arbitrage-bot
    if ($LASTEXITCODE -ne 0) {
      throw "cargo build failed"
    }
    $buildProfile = "debug"
  }

  New-Item -ItemType Directory -Force -Path $binariesDir | Out-Null
  New-Item -ItemType Directory -Force -Path $targetDir | Out-Null
  New-Item -ItemType Directory -Force -Path $coreContractDir | Out-Null

  $profileDir = Join-Path (Join-Path $targetDir $targetTriple) $buildProfile
  Copy-Item (Join-Path $profileDir "polymarket-arbitrage-bot.exe") $botOutput -Force
  # Desktop owns its runtime env template in this branch. We sync the core bot
  # code from the pinned ref, but keep desktop defaults versioned locally.

  @(
    "CORE_REF=$CoreRef",
    "CORE_REPO=$CoreRepo",
    "TARGET_TRIPLE=$targetTriple",
    "SOURCE_MODE=$sourceMode",
    "BUILD_PROFILE=$buildProfile",
    "PATCHES_SHA256=$patchHash",
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
