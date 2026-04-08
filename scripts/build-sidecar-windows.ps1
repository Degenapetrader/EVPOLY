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
if (-not $env:CARGO_NET_RETRY) {
  $env:CARGO_NET_RETRY = "10"
}
if (-not $env:CARGO_HTTP_TIMEOUT) {
  $env:CARGO_HTTP_TIMEOUT = "600"
}
if (-not $env:CARGO_REGISTRIES_CRATES_IO_PROTOCOL) {
  $env:CARGO_REGISTRIES_CRATES_IO_PROTOCOL = "sparse"
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
  if ([string]::IsNullOrWhiteSpace($Ref)) {
    return $false
  }

  try {
    & git rev-parse --verify ("{0}^{{commit}}" -f $Ref) *> $null
    return ($LASTEXITCODE -eq 0)
  }
  catch {
    return $false
  }
}

function Resolve-RemoteCoreRef([string]$Repo, [string]$Ref) {
  if ([string]::IsNullOrWhiteSpace($Ref)) {
    return $null
  }

  if ($Ref -notmatch '^[0-9a-fA-F]{7,40}$') {
    return $Ref
  }

  $normalizedRef = $Ref.ToLowerInvariant()
  try {
    foreach ($line in (& git ls-remote --heads --tags $Repo 2>$null)) {
      if ($line -match '^(?<sha>[0-9a-fA-F]{40})\s+(?<name>\S+)$') {
        $sha = $matches['sha'].ToLowerInvariant()
        if ($sha.StartsWith($normalizedRef)) {
          return $matches['name']
        }
      }
    }
  }
  catch {
    return $null
  }

  return $null
}

function Invoke-BestEffortNativeCommand {
  param(
    [string]$Label,
    [scriptblock]$Command,
    [int]$Attempts = 3,
    [int]$InitialDelaySeconds = 5
  )

  $delay = $InitialDelaySeconds
  for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
    try {
      & $Command
      if ($LASTEXITCODE -eq 0) {
        return $true
      }
      throw ("command exited {0}" -f $LASTEXITCODE)
    }
    catch {
      if ($attempt -ge $Attempts) {
        Write-Warning ("[build-sidecar-windows] {0} failed after {1} attempt(s): {2}" -f $Label, $Attempts, $_.Exception.Message)
        return $false
      }
      Write-Warning ("[build-sidecar-windows] {0} attempt {1}/{2} failed: {3}; retrying in {4}s" -f $Label, $attempt, $Attempts, $_.Exception.Message, $delay)
      Start-Sleep -Seconds $delay
      $delay = [Math]::Min($delay * 2, 30)
    }
  }

  return $false
}

function Ensure-LocalCoreRef([string]$Ref) {
  if (Test-GitCommitExists $Ref) {
    return $true
  }

  & git remote get-url origin *> $null
  if ($LASTEXITCODE -eq 0) {
    $remoteFetchRef = Resolve-RemoteCoreRef "origin" $Ref
    if ($remoteFetchRef) {
      Write-Host ("[build-sidecar-windows] fetching pinned core ref from origin via {0}" -f $remoteFetchRef)
      [void](Invoke-BestEffortNativeCommand -Label "git fetch pinned core ref" -Attempts 2 -InitialDelaySeconds 3 -Command {
        & git fetch --depth=1 origin $remoteFetchRef *> $null
      })
    }
  }

  return (Test-GitCommitExists $Ref)
}

try {
  if (Ensure-LocalCoreRef $CoreRef) {
    Write-Host ("[build-sidecar-windows] creating local worktree ref={0}" -f $CoreRef)
    git worktree add --detach $workDir $CoreRef
    if ($LASTEXITCODE -ne 0) {
      throw "git worktree add failed"
    }
    $useLocalWorktree = $true
    $sourceMode = "local-worktree"
  } else {
    Write-Host ("[build-sidecar-windows] cloning {0} ref={1}" -f $CoreRepo, $CoreRef)
    $cloneOk = Invoke-BestEffortNativeCommand -Label "git clone core repo" -Attempts 2 -InitialDelaySeconds 5 -Command {
      git clone --filter=blob:none $CoreRepo $workDir
    }
    if (-not $cloneOk) {
      throw "git clone failed"
    }

    if (-not (Invoke-BestEffortNativeCommand -Label "git checkout pinned core ref" -Attempts 2 -InitialDelaySeconds 3 -Command {
      git -C $workDir checkout --detach $CoreRef
    })) {
      $remoteFetchRef = Resolve-RemoteCoreRef $CoreRepo $CoreRef
      if (-not $remoteFetchRef) {
        throw ("unable to resolve remote ref for pinned core ref {0}" -f $CoreRef)
      }
      Write-Warning ("[build-sidecar-windows] direct checkout of pinned core ref failed after clone; fetching {0} and retrying" -f $remoteFetchRef)
      [void](Invoke-BestEffortNativeCommand -Label "git fetch pinned core ref fallback" -Attempts 2 -InitialDelaySeconds 3 -Command {
        git -C $workDir fetch --depth=1 origin $remoteFetchRef
      })
      git -C $workDir checkout --detach $CoreRef
    }
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
  $releaseBuildOk = Invoke-BestEffortNativeCommand -Label "cargo build release sidecar" -Attempts 3 -InitialDelaySeconds 10 -Command {
    cargo build --release --manifest-path $manifest --target-dir $targetDir --target $targetTriple --bin polymarket-arbitrage-bot
  }
  if (-not $releaseBuildOk) {
    if ($env:ALLOW_DEBUG_SIDECAR_FALLBACK -eq "1") {
      Write-Warning "[build-sidecar-windows] release build failed after retries; retrying debug fallback because ALLOW_DEBUG_SIDECAR_FALLBACK=1"
      $env:CARGO_BUILD_JOBS = "1"
      $debugBuildOk = Invoke-BestEffortNativeCommand -Label "cargo build debug sidecar" -Attempts 2 -InitialDelaySeconds 10 -Command {
        cargo build --manifest-path $manifest --target-dir $targetDir --target $targetTriple --bin polymarket-arbitrage-bot
      }
      if (-not $debugBuildOk) {
        throw "cargo build failed"
      }
      $buildProfile = "debug"
    } else {
      throw ("release sidecar build failed exit_code={0}; debug fallback is disabled" -f $LASTEXITCODE)
    }
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
