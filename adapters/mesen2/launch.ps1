# Windows launcher for the emucap Mesen adapter. It copies Mesen.exe into an emucap-owned portable
# directory, writes the adapter settings next to that copy, and launches it with the ROM + Lua.
#
# Usage:  powershell -ExecutionPolicy Bypass -File launch.ps1 <ROM> <EMUCAP_PORT> [NAME]
# Set MESEN_BIN to the full path of Mesen.exe if it is not in a common install path or PATH.

param(
  [Parameter(Mandatory = $true)][string]$Rom,
  [Parameter(Mandatory = $true)][ValidateRange(1, 65535)][int]$Port,
  [string]$Name = ""
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$lua  = Join-Path $here "emucap-live.lua"

function Get-LocalTcpConnections([int]$LocalPort) {
  if (-not (Get-Command Get-NetTCPConnection -ErrorAction SilentlyContinue)) {
    throw "Get-NetTCPConnection is unavailable; use the MCP launch tool or run this fallback on Windows with the NetTCPIP module"
  }
  @(Get-NetTCPConnection -LocalPort $LocalPort -ErrorAction SilentlyContinue)
}

function Get-EmulatorConnectionPids([int]$LocalPort) {
  $pids = @()
  foreach ($conn in @(Get-LocalTcpConnections $LocalPort | Where-Object { $_.State -eq "Established" })) {
    try {
      $proc = Get-Process -Id $conn.OwningProcess -ErrorAction Stop
      if ($proc.ProcessName -match '^(Mesen|mednafen|Flycast|pcsx-redux|mame)$') {
        $pids += $proc.Id
      }
    } catch {
    }
  }
  @($pids | Sort-Object -Unique)
}

function Test-ConnectedPid([int]$LocalPort, [int]$ProcessId) {
  @(Get-LocalTcpConnections $LocalPort | Where-Object {
    $_.State -eq "Established" -and $_.OwningProcess -eq $ProcessId
  }).Count -gt 0
}

function Stop-StartedProcess($Process) {
  try {
    $Process.Refresh()
    if (-not $Process.HasExited) {
      Stop-Process -Id $Process.Id -ErrorAction SilentlyContinue
    }
  } catch {
  }
}

function Replace-PortableFile([string]$Source, [string]$Destination) {
  $parent = Split-Path -Parent $Destination
  New-Item -ItemType Directory -Force -Path $parent | Out-Null
  if (Test-Path -LiteralPath $Destination -PathType Container) {
    throw "portable destination is a directory: $Destination"
  }
  $suffix = [System.Guid]::NewGuid().ToString("N")
  $tmp = "$Destination.tmp.$suffix"
  $backup = "$Destination.old.$suffix"
  $hadExisting = Test-Path -LiteralPath $Destination
  try {
    Copy-Item -LiteralPath $Source -Destination $tmp -Force
    if ($hadExisting) {
      Move-Item -LiteralPath $Destination -Destination $backup
    }
    Move-Item -LiteralPath $tmp -Destination $Destination
    if ($hadExisting -and (Test-Path -LiteralPath $backup)) {
      Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
    }
  } catch {
    if ((Test-Path -LiteralPath $backup) -and -not (Test-Path -LiteralPath $Destination)) {
      Move-Item -LiteralPath $backup -Destination $Destination -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $tmp) {
      Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    }
    throw
  }
}

function Get-MesenCandidatePaths {
  if ($env:MESEN_BIN) {
    $env:MESEN_BIN
  }

  foreach ($key in @("LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)", "USERPROFILE")) {
    $base = [Environment]::GetEnvironmentVariable($key)
    if ($base) {
      Join-Path $base "Programs\Mesen\Mesen.exe"
      Join-Path $base "Mesen\Mesen.exe"
    }
  }

  $cmd = Get-Command "Mesen.exe" -ErrorAction SilentlyContinue
  if ($cmd) {
    $cmd.Source
  }
}

if (-not (Test-Path -LiteralPath $Rom -PathType Leaf)) {
  throw "ROM not found: $Rom"
}
if (-not (Test-Path -LiteralPath $lua -PathType Leaf)) {
  throw "Lua adapter not found: $lua"
}

$sourceMesen = ""
foreach ($candidate in @(Get-MesenCandidatePaths)) {
  if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
    $sourceMesen = (Resolve-Path -LiteralPath $candidate).Path
    break
  }
}
if (-not $sourceMesen -or -not (Test-Path -LiteralPath $sourceMesen -PathType Leaf)) {
  throw "Mesen.exe not found; install Mesen in a common user/program-files path, add it to PATH, or set MESEN_BIN to the full path"
}

if ($env:EMUCAP_EMU_HOME) {
  $emuBase = $env:EMUCAP_EMU_HOME
} elseif ($env:LOCALAPPDATA) {
  $emuBase = Join-Path $env:LOCALAPPDATA "emucap"
} elseif ($env:APPDATA) {
  $emuBase = Join-Path $env:APPDATA "emucap"
} elseif ($env:USERPROFILE) {
  $emuBase = Join-Path $env:USERPROFILE "AppData\Local\emucap"
} else {
  $emuBase = Join-Path ([System.IO.Path]::GetTempPath()) "emucap"
}
$emuHome = Join-Path $emuBase "mesen2\$Port"
$portableDir = Join-Path $emuHome "portable"
$pidFile = Join-Path $emuHome "mesen.pid"
$log = if ($env:EMUCAP_LOG) { $env:EMUCAP_LOG } else { Join-Path $emuHome "mesen.log" }
$waitSeconds = if ($env:EMUCAP_LAUNCH_WAIT) { [int]$env:EMUCAP_LAUNCH_WAIT } else { 20 }
$postConnectGraceSeconds = if ($env:EMUCAP_POST_CONNECT_GRACE) { [int]$env:EMUCAP_POST_CONNECT_GRACE } else { 2 }
if ($waitSeconds -lt 0) {
  throw "EMUCAP_LAUNCH_WAIT must be 0 or greater"
}
if ($postConnectGraceSeconds -lt 0) {
  throw "EMUCAP_POST_CONNECT_GRACE must be 0 or greater"
}

$tcpConnections = Get-LocalTcpConnections $Port
$listeners = @($tcpConnections | Where-Object { $_.State -eq "Listen" })
if ($listeners.Count -eq 0) {
  throw "No MCP listener on port $Port; call emucap MCP status immediately before launching and pass status.listening_port"
}
$busy = @(Get-EmulatorConnectionPids $Port)
if ($busy.Count -gt 0) {
  throw "Port $Port already has an emulator connection (PID: $($busy -join ', ')); do not relaunch on a stale port"
}

New-Item -ItemType Directory -Force -Path $portableDir | Out-Null
$mesen = Join-Path $portableDir (Split-Path -Leaf $sourceMesen)
Replace-PortableFile $sourceMesen $mesen

$settings = Join-Path $portableDir "settings.json"
@'
{
  "Debug": {
    "ScriptWindow": {
      "AllowIoOsAccess": true,
      "AllowNetworkAccess": true,
      "ScriptTimeout": 60
    }
  },
  "Preferences": {
    "SingleInstance": false
  }
}
'@ | Set-Content $settings -Encoding UTF8

# Launch Mesen with the ROM + Lua; the Lua reads EMUCAP_PORT (and the rest) from the environment.
$env:EMUCAP_PORT    = "$Port"
$env:EMUCAP_CONTENT = $Rom
if ($Name) { $env:EMUCAP_NAME = $Name }
$buildHash = "unknown"
try {
  $rev = (& git -C $here rev-parse --short HEAD 2>$null)
  if ($LASTEXITCODE -eq 0 -and $rev) {
    $buildHash = ($rev | Select-Object -First 1).Trim()
  }
  & git -C $here diff --quiet HEAD -- emucap-live.lua 2>$null
  if ($LASTEXITCODE -ne 0) {
    $buildHash = "$buildHash-dirty"
  }
} catch {
}
$env:EMUCAP_BUILD_HASH = $buildHash
$tokenFile = Join-Path ([System.IO.Path]::GetTempPath()) "emucap_session_token_$Port"
if (-not $env:EMUCAP_SESSION_TOKEN -and (Test-Path -LiteralPath $tokenFile -PathType Leaf)) {
  $env:EMUCAP_SESSION_TOKEN = (Get-Content -LiteralPath $tokenFile -TotalCount 1).Trim()
}
@(
  "emucap Mesen Windows launch",
  "  rom=$Rom",
  "  port=$Port",
  "  name=$Name",
  "  lua=$lua",
  "  source_mesen_bin=$sourceMesen",
  "  portable_mesen_bin=$mesen",
  "  portable_settings=$settings",
  "  build_hash=$buildHash",
  "  wait=${waitSeconds}s",
  "  post_connect_grace=${postConnectGraceSeconds}s"
) | Set-Content -LiteralPath $log -Encoding UTF8
$started = Start-Process -FilePath $mesen -ArgumentList @($Rom, $lua) -WorkingDirectory $portableDir -PassThru
Set-Content -LiteralPath $pidFile -Value "$($started.Id)" -Encoding ASCII

$connectedPid = $null
$deadline = (Get-Date).AddSeconds($waitSeconds)
while ($true) {
  $connectionPids = @(Get-EmulatorConnectionPids $Port)
  if ($connectionPids.Count -gt 0) {
    $connectedPid = $connectionPids[0]
    break
  }
  try {
    $started.Refresh()
    if ($started.HasExited) {
      throw "Mesen exited before connecting to MCP (pid=$($started.Id)); see log=$log"
    }
  } catch {
    throw
  }
  if ((Get-Date) -ge $deadline) {
    break
  }
  Start-Sleep -Seconds 1
}

if (-not $connectedPid) {
  Stop-StartedProcess $started
  throw "Mesen did not connect to MCP port $Port within ${waitSeconds}s; see log=$log"
}

Set-Content -LiteralPath $pidFile -Value "$connectedPid" -Encoding ASCII
if ($postConnectGraceSeconds -gt 0) {
  Start-Sleep -Seconds $postConnectGraceSeconds
  if (-not (Test-ConnectedPid $Port $connectedPid)) {
    Stop-StartedProcess $started
    throw "Mesen lost the MCP connection right after launch (pid=$connectedPid); see log=$log"
  }
}

Write-Host "Mesen connected: pid=$connectedPid port=$Port rom=$Rom lua=$lua portable=$portableDir log=$log"
