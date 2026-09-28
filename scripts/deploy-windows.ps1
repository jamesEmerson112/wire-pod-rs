<#
Puts the Rust chipper.exe in place of the installed Go one, or puts Go back.

Run from an elevated PowerShell, as the same Windows user the tray runs as,
because the tray keeps its state under that user's HKCU:

  powershell -ExecutionPolicy Bypass -File scripts\deploy-windows.ps1
  powershell -ExecutionPolicy Bypass -File scripts\deploy-windows.ps1 -Rollback

Build first with `bash scripts/gate-packaged.sh`.

The first deploy copies the Go binary to chipper-go.exe beside it and never
overwrites that copy afterwards. Only chipper.exe and the version file change;
the firewall rule, the Run key, the uninstaller, the shortcuts, the DLLs and the
assets all name the same path and stay as they are.

Modelled on the fork's build-windows.ps1 -Deploy, except that it never stops a
process by image name: the development server is also called chipper.exe.
Only processes whose executable is the installed chipper.exe are stopped.

The server it starts inherits this shell's elevation, as the fork's script does.
#>
param(
    [switch]$Rollback,
    [string]$InstallDir = 'C:\Program Files\wire-pod\chipper',
    [string]$Build = ''
)

$ErrorActionPreference = 'Stop'

if (-not $Build) {
    $targetDir = $env:CARGO_TARGET_DIR
    if (-not $targetDir) { $targetDir = 'E:\GitHub\wire-pod-rs-target-gate' }
    $Build = Join-Path $targetDir 'release\chipper.exe'
}

$exe = Join-Path $InstallDir 'chipper.exe'
$goExe = Join-Path $InstallDir 'chipper-go.exe'
$versionFile = Join-Path $InstallDir 'version'
$goVersionFile = Join-Path $InstallDir 'version-go'
$softwareKey = 'HKCU:\Software\wire-pod'

function Assert-Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run this from an elevated PowerShell: it writes under Program Files.'
    }
}

# The PE optional header's Subsystem field: 2 is the GUI subsystem, which only
# the tray build has, so a console build can never be installed by mistake.
function Test-GuiSubsystem([string]$path) {
    $bytes = [IO.File]::ReadAllBytes($path)
    $peOffset = [BitConverter]::ToInt32($bytes, 0x3C)
    $subsystem = [BitConverter]::ToUInt16($bytes, $peOffset + 4 + 20 + 68)
    return $subsystem -eq 2
}

function Stop-InstalledServer {
    $running = @(Get-CimInstance Win32_Process -Filter "Name = 'chipper.exe'" |
        Where-Object { $_.ExecutablePath -and ($_.ExecutablePath -ieq $exe) })
    foreach ($process in $running) {
        Write-Host ("Stopping the installed server, PID {0}" -f $process.ProcessId)
        Stop-Process -Id $process.ProcessId -Force
        Wait-Process -Id $process.ProcessId -Timeout 15 -ErrorAction SilentlyContinue
    }
    if ($running.Count -eq 0) {
        Write-Host 'The installed server is not running.'
    }
}

# A stopped process can hold its image for a moment after Wait-Process returns,
# so the copy over it is retried for up to ten seconds.
function Copy-OverServer([string]$from, [string]$to) {
    for ($attempt = 1; ; $attempt++) {
        try {
            Copy-Item -Path $from -Destination $to -Force
            return
        } catch {
            if ($attempt -ge 20) { throw }
            Start-Sleep -Milliseconds 500
        }
    }
}

function Get-WebPort {
    $port = '8080'
    $props = Get-ItemProperty -Path $softwareKey -ErrorAction SilentlyContinue
    if ($props -and $props.WebPort -and $props.WebPort -ne '0') { $port = $props.WebPort }
    return $port
}

function Start-InstalledServer {
    # As the Run key starts it: `chipper.exe -d`, which skips the start-up box.
    Start-Process -FilePath $exe -ArgumentList '-d' -WorkingDirectory $InstallDir
    $url = 'http://localhost:{0}/api/is_running' -f (Get-WebPort)
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        try {
            $answer = Invoke-WebRequest -Uri $url -UseBasicParsing -TimeoutSec 3
            if ($answer.Content.Trim() -eq 'true') {
                Write-Host "is_running answered true at $url"
                return
            }
        } catch {
            Start-Sleep -Seconds 1
        }
    }
    throw "is_running did not answer true at $url within 30 seconds"
}

Assert-Elevated
if (-not (Test-Path $exe)) { throw "No installed chipper.exe at $exe" }

if ($Rollback) {
    if (-not (Test-Path $goExe)) { throw "No Go backup at $goExe; nothing to roll back to." }
    Stop-InstalledServer
    Copy-OverServer $goExe $exe
    if (Test-Path $goVersionFile) { Copy-Item -Path $goVersionFile -Destination $versionFile -Force }
    Write-Host 'The Go server is back in place.'
} else {
    if (-not (Test-Path $Build)) { throw "No build at $Build; run scripts/gate-packaged.sh first." }
    if (-not (Test-GuiSubsystem $Build)) { throw "$Build is not a tray build; build with --features stt-vosk,tray." }
    Stop-InstalledServer
    if (-not (Test-Path $goExe)) {
        Copy-Item -Path $exe -Destination $goExe
        Write-Host "Kept the Go binary as $goExe"
    }
    if ((Test-Path $versionFile) -and -not (Test-Path $goVersionFile)) {
        Copy-Item -Path $versionFile -Destination $goVersionFile
    }
    Copy-OverServer $Build $exe
    if (Test-Path $goVersionFile) {
        $goVersion = ([IO.File]::ReadAllText($goVersionFile)).Trim()
        [IO.File]::WriteAllText($versionFile, "$goVersion-rs")
    }
    Write-Host 'The Rust server is in place.'
}

Start-InstalledServer
