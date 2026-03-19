param(
  [string]$BotBin = "src-tauri\binaries\evpoly-bot-x86_64-pc-windows-msvc.exe",
  [int]$TimeoutSeconds = 15
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $BotBin)) {
  throw "bot binary not found: $BotBin"
}

$workDir = Join-Path $env:RUNNER_TEMP ("evpoly-smoke-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $workDir | Out-Null

try {
  if (-not $env:POLY_PRIVATE_KEY) {
    $env:POLY_PRIVATE_KEY = "0x1111111111111111111111111111111111111111111111111111111111111111"
  }
  if (-not $env:POLY_PROXY_WALLET_ADDRESS) {
    $env:POLY_PROXY_WALLET_ADDRESS = "0x1111111111111111111111111111111111111111"
  }
  if (-not $env:POLY_SIGNATURE_TYPE) {
    $env:POLY_SIGNATURE_TYPE = "1"
  }

  $patterns = @(
    "missing field 'check_interval_ms'",
    "unexpected argument '--env-file'",
    "unexpected argument '--no-simulation'",
    "error: unexpected argument",
    "command create_profile missing required key walletAddress",
    "thread '.*' panicked",
    "panicked at"
  )

  function Run-SmokeMode {
    param(
      [string]$Mode,
      [string]$Flag
    )

    $cfg = Join-Path $workDir ("runtime-" + $Mode + ".config.json")
    $log = Join-Path $workDir ("bot-" + $Mode + ".log")
    if (Test-Path $cfg) { Remove-Item $cfg -Force }
    if (Test-Path $log) { Remove-Item $log -Force }

    $proc = Start-Process -FilePath $BotBin -ArgumentList @("--config", $cfg, $Flag) -RedirectStandardOutput $log -RedirectStandardError $log -PassThru -NoNewWindow
    $exited = $proc.WaitForExit($TimeoutSeconds * 1000)
    if (-not $exited) {
      Stop-Process -Id $proc.Id -Force
    }

    if (-not (Test-Path $cfg)) {
      throw ("expected config file was not created for mode={0}: {1}" -f $Mode, $cfg)
    }

    foreach ($pat in $patterns) {
      if (Select-String -Path $log -Pattern $pat -Quiet) {
        Write-Host "blocked pattern matched: $pat"
        Get-Content -Path $log -TotalCount 200
        throw "smoke failed for mode=$Mode"
      }
    }

    Write-Host "smoke ok: mode=$Mode"
  }

  Run-SmokeMode -Mode "simulation" -Flag "--simulation"
  Run-SmokeMode -Mode "live" -Flag "--no-simulation"
  Write-Host "all smoke checks passed"
}
finally {
  if (Test-Path $workDir) {
    Remove-Item -Path $workDir -Recurse -Force
  }
}
