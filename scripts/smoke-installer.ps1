param(
  [string]$BundleDir = "src-tauri\target\release\bundle\nsis",
  [int]$LaunchSeconds = 8
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true

$installer = Get-ChildItem -Path $BundleDir -Filter "*setup.exe" -File |
  Sort-Object LastWriteTimeUtc -Descending |
  Select-Object -First 1

if (-not $installer) {
  throw "installer not found under $BundleDir"
}

$installDir = Join-Path $env:LOCALAPPDATA "EVPoly"
$desktopExe = Join-Path $installDir "evpoly-desktop.exe"
$sidecarExe = Join-Path $installDir "evpoly-bot.exe"

$runningDesktop = Get-Process -Name "evpoly-desktop" -ErrorAction SilentlyContinue
if ($runningDesktop) {
  $runningDesktop | Stop-Process -Force
}

$uninstaller = $null
$launched = $null

try {
  Write-Host ("[smoke-installer] installing {0}" -f $installer.FullName)
  Start-Process -FilePath $installer.FullName -ArgumentList "/S" -Wait

  if (-not (Test-Path $desktopExe)) {
    throw "desktop binary not installed: $desktopExe"
  }
  if (-not (Test-Path $sidecarExe)) {
    throw "sidecar binary not installed: $sidecarExe"
  }

  Write-Host ("[smoke-installer] launching {0}" -f $desktopExe)
  $launched = Start-Process -FilePath $desktopExe -PassThru
  Start-Sleep -Seconds $LaunchSeconds
  $launched.Refresh()
  if ($launched.HasExited) {
    throw ("desktop process exited too early with code {0}" -f $launched.ExitCode)
  }

  $uninstaller = Get-ChildItem -Path $installDir -Filter "uninstall*.exe" -File -ErrorAction SilentlyContinue |
    Select-Object -First 1
  if (-not $uninstaller) {
    throw "uninstaller not found after install"
  }
}
finally {
  if ($launched -and -not $launched.HasExited) {
    Stop-Process -Id $launched.Id -Force -ErrorAction SilentlyContinue
  }
  if ($uninstaller) {
    Write-Host ("[smoke-installer] uninstalling {0}" -f $uninstaller.FullName)
    Start-Process -FilePath $uninstaller.FullName -ArgumentList "/S" -Wait
    Start-Sleep -Seconds 2
  }
}

if (Test-Path $desktopExe) {
  throw "desktop binary still present after uninstall"
}

Write-Host "[smoke-installer] installer smoke passed"
