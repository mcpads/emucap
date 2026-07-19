# Windows launcher for the emucap Mesen adapter. It copies Mesen.exe into an emucap-owned portable
# directory, writes the adapter settings next to that copy, and launches it with the ROM + Lua.
#
# Usage:  powershell -ExecutionPolicy Bypass -File launch.ps1 <ROM> <EMUCAP_PORT> [NAME] [SYSTEM]
# Set MESEN_BIN to the full path of Mesen.exe if it is not in a common install path or PATH.
# Set EMUCAP_MESEN_LUA to override the per-system entry selected from SYSTEM or the ROM extension.

param(
  [Parameter(Mandatory = $true)][string]$Rom,
  [Parameter(Mandatory = $true)][ValidateRange(1, 65535)][int]$Port,
  [string]$Name = "",
  [string]$System = ""
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

function Resolve-MesenLuaEntry([string]$RequestedSystem, [string]$ContentPath) {
  if ($RequestedSystem) {
    switch ($RequestedSystem.Trim().ToLowerInvariant()) {
      "snes" { return (Join-Path $here "emucap-snes.lua") }
      "gamegear" { return (Join-Path $here "emucap-sms.lua") }
      "gb" { return (Join-Path $here "emucap-gb.lua") }
      "gbc" { return (Join-Path $here "emucap-gb.lua") }
      "gba" { return (Join-Path $here "emucap-gba.lua") }
      "nes" { return (Join-Path $here "emucap-nes.lua") }
      default { throw "unsupported Mesen system: $RequestedSystem" }
    }
  }

  switch ([System.IO.Path]::GetExtension($ContentPath).ToLowerInvariant()) {
    { $_ -in @(".sfc", ".smc") } { return (Join-Path $here "emucap-snes.lua") }
    { $_ -in @(".gg", ".sms") } { return (Join-Path $here "emucap-sms.lua") }
    { $_ -in @(".gb", ".gbc") } { return (Join-Path $here "emucap-gb.lua") }
    ".gba" { return (Join-Path $here "emucap-gba.lua") }
    ".nes" { return (Join-Path $here "emucap-nes.lua") }
    default {
      throw "cannot infer the Mesen system from ROM extension: $ContentPath; pass SYSTEM, set EMUCAP_MESEN_LUA, or use MCP launch(content_path, system)"
    }
  }
}

$lua = if ($env:EMUCAP_MESEN_LUA) {
  $env:EMUCAP_MESEN_LUA
} else {
  Resolve-MesenLuaEntry $System $Rom
}

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

function Replace-PortableDirectory([string]$Source, [string]$Destination) {
  $parent = Split-Path -Parent $Destination
  New-Item -ItemType Directory -Force -Path $parent | Out-Null
  $suffix = [System.Guid]::NewGuid().ToString("N")
  $tmp = "$Destination.tmp.$suffix"
  $backup = "$Destination.old.$suffix"
  $hadExisting = Test-Path -LiteralPath $Destination
  try {
    Copy-Item -LiteralPath $Source -Destination $tmp -Recurse
    if ($hadExisting) { Move-Item -LiteralPath $Destination -Destination $backup }
    Move-Item -LiteralPath $tmp -Destination $Destination
    if ($hadExisting -and (Test-Path -LiteralPath $backup)) {
      Remove-Item -LiteralPath $backup -Recurse -Force -ErrorAction SilentlyContinue
    }
  } catch {
    if ((Test-Path -LiteralPath $backup) -and -not (Test-Path -LiteralPath $Destination)) {
      Move-Item -LiteralPath $backup -Destination $Destination -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $tmp) {
      Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
    throw
  }
}

function Get-MesenCandidatePaths {
  if ($env:MESEN_BIN) {
    $env:MESEN_BIN
  }

  Join-Path $here "work\mesen\bin\win-x64\Release\Mesen.exe"

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
$lua = (Resolve-Path -LiteralPath $lua).Path

$sourceMesen = ""
foreach ($candidate in @(Get-MesenCandidatePaths)) {
  if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
    $sourceMesen = (Resolve-Path -LiteralPath $candidate).Path
    break
  }
}
if (-not $sourceMesen -or -not (Test-Path -LiteralPath $sourceMesen -PathType Leaf)) {
  throw "compatible Mesen.exe not found; run adapters/mesen2/build.ps1 or set MESEN_BIN"
}

$metadataPath = Join-Path (Split-Path -Parent $sourceMesen) "emucap-mesen-build.json"
if (-not (Test-Path -LiteralPath $metadataPath -PathType Leaf)) {
  throw "mesen-patch-required: compatible sidecar missing at $metadataPath; run adapters/mesen2/build.ps1"
}
$metadata = Get-Content -Raw -LiteralPath $metadataPath | ConvertFrom-Json
$lockValues = @{}
foreach ($line in Get-Content -LiteralPath (Join-Path $here "upstream.lock")) {
  if ($line -match '^([^=]+)=(.*)$') { $lockValues[$Matches[1]] = $Matches[2] }
}
if ($metadata.upstream -ne $lockValues.MESEN_REPO -or
    $metadata.tag -ne $lockValues.MESEN_TAG -or
    $metadata.commit -ne $lockValues.MESEN_COMMIT -or
    [int]$metadata.host_api -ne [int]$lockValues.MESEN_HOST_API -or
    $metadata.patchset_sha256 -ne $lockValues.MESEN_PATCHSET_SHA256 -or
    $metadata.patchset_sha256 -notmatch '^[0-9a-fA-F]{64}$') {
  throw "mesen-patch-required: $metadataPath does not match upstream.lock"
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
$sourceDir = (Resolve-Path -LiteralPath (Split-Path -Parent $sourceMesen)).Path
$portableFull = [System.IO.Path]::GetFullPath($portableDir)
if ($portableFull.Equals($sourceDir, [System.StringComparison]::OrdinalIgnoreCase) -or
    $portableFull.StartsWith($sourceDir + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
  throw "portable destination must not be inside source publish directory: $portableFull"
}
Replace-PortableDirectory $sourceDir $portableDir

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

$isGba = ((Split-Path -Leaf $lua) -eq "emucap-gba.lua") -or ([System.IO.Path]::GetExtension($Rom) -ieq ".gba")
if ($isGba) {
  $firmwareDir = Join-Path $portableDir "Firmware"
  $firmwareDestination = Join-Path $firmwareDir "gba_bios.bin"
  $explicitBios = $env:EMUCAP_GBA_BIOS
  $biosSource = if ($explicitBios) { $explicitBios } else { Join-Path $emuBase "firmware\gba_bios.bin" }
  $stagedIsValid = (Test-Path -LiteralPath $firmwareDestination -PathType Leaf) -and
    ((Get-Item -LiteralPath $firmwareDestination).Length -eq 16384)
  if ($explicitBios -or -not $stagedIsValid) {
    if (-not (Test-Path -LiteralPath $biosSource -PathType Leaf)) {
      throw "GBA needs gba_bios.bin: set EMUCAP_GBA_BIOS or place it at $biosSource"
    }
    $biosSize = (Get-Item -LiteralPath $biosSource).Length
    if ($biosSize -ne 16384) {
      throw "GBA BIOS must be exactly 16384 bytes: $biosSource ($biosSize bytes)"
    }
    New-Item -ItemType Directory -Force -Path $firmwareDir | Out-Null
    Replace-PortableFile $biosSource $firmwareDestination
  }
}

# Launch Mesen with the ROM + Lua; the Lua reads EMUCAP_PORT (and the rest) from the environment.
$env:EMUCAP_ADAPTER_DIR = $here
$env:EMUCAP_PORT = "$Port"
$env:EMUCAP_CONTENT = $Rom
$env:EMUCAP_MESEN_UPSTREAM_COMMIT = [string]$metadata.commit
$env:EMUCAP_MESEN_PATCHSET_SHA256 = [string]$metadata.patchset_sha256
if ($Name) { $env:EMUCAP_NAME = $Name }
$buildHash = "unknown"
try {
  $rev = (& git -C $here rev-parse --short HEAD 2>$null)
  if ($LASTEXITCODE -eq 0 -and $rev) {
    $buildHash = ($rev | Select-Object -First 1).Trim()
  }
  $luaDirectory = [System.IO.Path]::GetFullPath((Split-Path -Parent $lua))
  $adapterDirectory = [System.IO.Path]::GetFullPath($here)
  if (-not $luaDirectory.Equals($adapterDirectory, [System.StringComparison]::OrdinalIgnoreCase)) {
    $buildHash = "$buildHash-dirty"
  } else {
    $entryName = Split-Path -Leaf $lua
    & git -C $here diff --quiet HEAD -- emucap-core.lua emucap_tx.lua emucap_state_io.lua $entryName 2>$null
    if ($LASTEXITCODE -ne 0) {
      $buildHash = "$buildHash-dirty"
    }
  }
} catch {
}
$env:EMUCAP_BUILD_HASH = $buildHash
$tokenFile = if ($env:EMUCAP_SESSION_TOKEN_FILE) {
  $env:EMUCAP_SESSION_TOKEN_FILE
} else {
  $runtimeBase = if ($env:EMUCAP_EMU_HOME) {
    $env:EMUCAP_EMU_HOME
  } elseif ($env:LOCALAPPDATA) {
    Join-Path $env:LOCALAPPDATA "emucap"
  } else {
    Join-Path ([System.IO.Path]::GetTempPath()) "emucap"
  }
  Join-Path $runtimeBase "sessions\compatibility\session-token-$Port"
}
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
$mesenArgs = @(
  $Rom,
  $lua,
  "--debug.scriptWindow.allowIoOsAccess=true",
  "--debug.scriptWindow.allowNetworkAccess=true",
  "--debug.scriptWindow.scriptTimeout=60",
  "--preferences.singleInstance=false",
  "--snes.port1.type=SnesController",
  "--donotSaveSettings"
)
$started = Start-Process -FilePath $mesen -ArgumentList $mesenArgs -WorkingDirectory $portableDir -PassThru
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
