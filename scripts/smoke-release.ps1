param(
  [string]$BotBin = "src-tauri\binaries\evpoly-bot-x86_64-pc-windows-msvc.exe",
  [string]$ManualBotBin = "src-tauri\binaries\evpoly-manual-bot-x86_64-pc-windows-msvc.exe",
  [int]$TimeoutSeconds = 15
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $BotBin)) {
  throw "bot binary not found: $BotBin"
}
if (-not (Test-Path $ManualBotBin)) {
  throw "manual bot binary not found: $ManualBotBin"
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
    $stdoutLog = Join-Path $workDir ("bot-" + $Mode + ".stdout.log")
    $stderrLog = Join-Path $workDir ("bot-" + $Mode + ".stderr.log")
    $log = Join-Path $workDir ("bot-" + $Mode + ".log")
    if (Test-Path $cfg) { Remove-Item $cfg -Force }
    if (Test-Path $stdoutLog) { Remove-Item $stdoutLog -Force }
    if (Test-Path $stderrLog) { Remove-Item $stderrLog -Force }
    if (Test-Path $log) { Remove-Item $log -Force }

    $proc = Start-Process -FilePath $BotBin -ArgumentList @("--config", $cfg, $Flag) -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog -PassThru -NoNewWindow
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

  function Run-ManualServiceSmoke {
    param(
      [string]$ConfigPath
    )

    $manualPort = 18000 + (Get-Random -Minimum 0 -Maximum 20000)
    $manualToken = "smoke-" + [Guid]::NewGuid().ToString("N")
    $manualOut = Join-Path $workDir "manual.stdout.log"
    $manualErr = Join-Path $workDir "manual.stderr.log"
    $manualLog = Join-Path $workDir "manual.log"
    if (Test-Path $manualOut) { Remove-Item $manualOut -Force }
    if (Test-Path $manualErr) { Remove-Item $manualErr -Force }
    if (Test-Path $manualLog) { Remove-Item $manualLog -Force }

    $env:EVPOLY_MANUAL_BOT_TOKEN = $manualToken
    $manualProc = Start-Process -FilePath $ManualBotBin -ArgumentList @("--config", $ConfigPath, "--bind", "127.0.0.1", "--port", $manualPort.ToString(), "--simulation") -RedirectStandardOutput $manualOut -RedirectStandardError $manualErr -PassThru -NoNewWindow

    $ready = $false
    try {
      for ($i = 0; $i -lt 40; $i++) {
        try {
          $response = Invoke-RestMethod -Uri ("http://127.0.0.1:{0}/manual/health" -f $manualPort) -Headers @{ "x-evpoly-manual-token" = $manualToken } -Method Get -TimeoutSec 2
          if ($response.ok -eq $true) {
            $ready = $true
            break
          }
        } catch {
          # keep polling
        }
        Start-Sleep -Milliseconds 500
      }
    } finally {
      if (-not $manualProc.HasExited) {
        Stop-Process -Id $manualProc.Id -Force
      }
      Remove-Item Env:EVPOLY_MANUAL_BOT_TOKEN -ErrorAction SilentlyContinue
    }

    if (-not (Test-Path $manualOut)) { New-Item -Path $manualOut -ItemType File | Out-Null }
    if (-not (Test-Path $manualErr)) { New-Item -Path $manualErr -ItemType File | Out-Null }
    Get-Content -Path $manualOut, $manualErr | Set-Content -Path $manualLog

    if (-not $ready) {
      Get-Content -Path $manualLog -TotalCount 200
      throw "manual service smoke health check failed"
    }

    foreach ($pat in $patterns) {
      if (Select-String -Path $manualLog -Pattern $pat -Quiet) {
        Write-Host "blocked pattern matched in manual service log: $pat"
        Get-Content -Path $manualLog -TotalCount 200
        throw "manual service smoke failed"
      }
    }

    Write-Host "smoke ok: manual service"
  }

  Run-SmokeMode -Mode "simulation" -Flag "--simulation"
  Run-SmokeMode -Mode "live" -Flag "--no-simulation"
  Run-ManualServiceSmoke -ConfigPath (Join-Path $workDir "runtime-simulation.config.json")
  Write-Host "all smoke checks passed"
}
finally {
  if (Test-Path $workDir) {
    Remove-Item -Path $workDir -Recurse -Force
  }
}
