param(
  [string]$BotBin = "src-tauri\binaries\evpoly-bot-x86_64-pc-windows-msvc.exe",
  [int]$TimeoutSeconds = 15
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $BotBin)) {
  throw "bot binary not found: $BotBin"
}

$workRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$workDir = Join-Path $workRoot ("evpoly-smoke-" + [Guid]::NewGuid().ToString("N"))
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
    $stdoutLog = Join-Path $workDir ("bot-" + $Mode + ".stdout.log")
    $stderrLog = Join-Path $workDir ("bot-" + $Mode + ".stderr.log")
    $log = Join-Path $workDir ("bot-" + $Mode + ".log")
    if (Test-Path $cfg) { Remove-Item $cfg -Force }
    if (Test-Path $stdoutLog) { Remove-Item $stdoutLog -Force }
    if (Test-Path $stderrLog) { Remove-Item $stderrLog -Force }
    if (Test-Path $log) { Remove-Item $log -Force }

    $proc = Start-Process -FilePath $BotBin -ArgumentList @("--config", ('"{0}"' -f $cfg), $Flag) -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog -PassThru -NoNewWindow
    $exited = $proc.WaitForExit($TimeoutSeconds * 1000)
    if (-not $exited) {
      Stop-Process -Id $proc.Id -Force
    }

    if (-not (Test-Path $stdoutLog)) { New-Item -Path $stdoutLog -ItemType File | Out-Null }
    if (-not (Test-Path $stderrLog)) { New-Item -Path $stderrLog -ItemType File | Out-Null }
    Get-Content -Path $stdoutLog, $stderrLog | Set-Content -Path $log

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
    for ($attempt = 1; $attempt -le 5; $attempt++) {
      try {
        Remove-Item -Path $workDir -Recurse -Force
        break
      } catch {
        if ($attempt -eq 5) {
          throw
        }
        Start-Sleep -Milliseconds (250 * $attempt)
      }
    }
  }
}
