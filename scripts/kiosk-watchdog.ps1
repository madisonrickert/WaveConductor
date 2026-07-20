# WaveConductor kiosk watchdog.
#
# Keeps waveconductor.exe running unattended: restarts it on exit (crash,
# panic, GPU device loss) with exponential backoff, kills and restarts it if
# the window stops responding (hang detection via the process Responding
# flag - no app-side changes required), and rotates its own log so multi-day
# runs cannot fill the disk.
#
# Usage (from the directory containing waveconductor.exe):
#   powershell -ExecutionPolicy Bypass -File kiosk-watchdog.ps1            # run
#   powershell -ExecutionPolicy Bypass -File kiosk-watchdog.ps1 -Install  # register logon task
#   powershell -ExecutionPolicy Bypass -File kiosk-watchdog.ps1 -Uninstall
#
# See docs/runbooks/kiosk.md for the full deployment checklist.

param(
    [switch]$Install,
    [switch]$Uninstall,
    # Seconds the window may be continuously unresponsive before a forced restart.
    [int]$HangSeconds = 60,
    # Restart backoff bounds (seconds). Backoff doubles per rapid failure and
    # resets once the app has stayed up for $HealthySeconds.
    [int]$BackoffMin = 5,
    [int]$BackoffMax = 60,
    [int]$HealthySeconds = 600
)

$ErrorActionPreference = 'Stop'
$TaskName = 'WaveConductorKiosk'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$Exe = Join-Path $Root 'waveconductor.exe'
$Log = Join-Path $Root 'kiosk-watchdog.log'
$LogMaxBytes = 5MB

function Write-Log([string]$msg) {
    $line = "{0:yyyy-MM-dd HH:mm:ss}  {1}" -f (Get-Date), $msg
    # Size-capped rotation: keep exactly one previous generation (.1).
    if ((Test-Path $Log) -and (Get-Item $Log).Length -gt $LogMaxBytes) {
        Move-Item -Force $Log "$Log.1"
    }
    Add-Content -Path $Log -Value $line
    Write-Host $line
}

if ($Install) {
    $action = "powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$($MyInvocation.MyCommand.Path)`""
    schtasks /Create /F /TN $TaskName /SC ONLOGON /RL LIMITED /TR $action | Out-Null
    Write-Log "installed Task Scheduler logon task '$TaskName'"
    return
}
if ($Uninstall) {
    schtasks /Delete /F /TN $TaskName | Out-Null
    Write-Log "removed Task Scheduler task '$TaskName'"
    return
}

if (-not (Test-Path $Exe)) {
    Write-Log "FATAL: $Exe not found - place this script beside waveconductor.exe"
    exit 1
}

$backoff = $BackoffMin
Write-Log "watchdog started (hang threshold ${HangSeconds}s, backoff ${BackoffMin}-${BackoffMax}s)"

while ($true) {
    $started = Get-Date
    Write-Log "launching $Exe"
    $proc = Start-Process -FilePath $Exe -WorkingDirectory $Root -PassThru

    # Supervise: poll for exit and for a continuously-unresponsive window.
    $unresponsiveSince = $null
    while (-not $proc.HasExited) {
        Start-Sleep -Seconds 5
        $proc.Refresh()
        if ($proc.HasExited) { break }
        if ($proc.Responding) {
            $unresponsiveSince = $null
        } elseif ($null -eq $unresponsiveSince) {
            $unresponsiveSince = Get-Date
        } elseif (((Get-Date) - $unresponsiveSince).TotalSeconds -ge $HangSeconds) {
            Write-Log "window unresponsive ${HangSeconds}s - forcing restart"
            try { Stop-Process -Id $proc.Id -Force -Confirm:$false } catch {}
            break
        }
    }

    $uptime = ((Get-Date) - $started).TotalSeconds
    $code = if ($proc.HasExited) { $proc.ExitCode } else { 'killed' }
    Write-Log ("app ended after {0:N0}s (exit {1})" -f $uptime, $code)

    # A long healthy run earns a fresh backoff; rapid failures double it.
    if ($uptime -ge $HealthySeconds) { $backoff = $BackoffMin }
    else { $backoff = [Math]::Min($backoff * 2, $BackoffMax) }

    Write-Log "restarting in ${backoff}s"
    Start-Sleep -Seconds $backoff
}
