# Build the pinned Dolphin native adapter on Windows.
#
# Requirements: Visual Studio 2022 with MSVC and a Windows SDK, plus Git.
#
#   build.ps1 [-Src <source-directory>] [-Jobs 2]
param(
  [string]$Src = "",
  [ValidateRange(1, 64)]
  [int]$Jobs = 2
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $Src) { $Src = Join-Path $here "work\dolphin-src" }

$lock = @{}
foreach ($line in Get-Content -LiteralPath (Join-Path $here "upstream.lock")) {
  if ($line -match '^([^=]+)=(.*)$') {
    $lock[$matches[1]] = $matches[2]
  }
}
foreach ($key in @("DOLPHIN_REPO", "DOLPHIN_COMMIT", "DOLPHIN_HOST_API", "DOLPHIN_PATCHSET_SHA256")) {
  if (-not $lock.ContainsKey($key) -or -not $lock[$key]) {
    throw "upstream.lock is missing $key"
  }
}

if (-not (Test-Path -LiteralPath (Join-Path $Src ".git") -PathType Container)) {
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Src) | Out-Null
  git clone --filter=blob:none $lock.DOLPHIN_REPO $Src
  if ($LASTEXITCODE -ne 0) { throw "failed to clone Dolphin" }
}

git -C $Src fetch --depth 1 origin $lock.DOLPHIN_COMMIT
if ($LASTEXITCODE -ne 0) { throw "failed to fetch the pinned Dolphin revision" }
git -C $Src checkout --detach $lock.DOLPHIN_COMMIT
if ($LASTEXITCODE -ne 0) { throw "failed to check out the pinned Dolphin revision" }
git -C $Src submodule update --init --recursive --depth 1 --jobs 4
if ($LASTEXITCODE -ne 0) { throw "failed to update Dolphin submodules" }

$owned = @(
  "Source/Core/AudioCommon/AudioCommon.cpp",
  "Source/Core/AudioCommon/AudioCommon.h",
  "Source/Core/AudioCommon/Mixer.cpp",
  "Source/Core/AudioCommon/Mixer.h",
  "Source/Core/VideoCommon/Present.cpp",
  "Source/Core/VideoCommon/FramebufferManager.cpp",
  "Source/Core/VideoCommon/FramebufferManager.h",
  "Source/Core/VideoCommon/FramebufferShaderGen.cpp",
  "Source/Core/VideoCommon/FramebufferShaderGen.h",
  "Source/Core/VideoCommon/Present.h",
  "Source/Core/VideoCommon/VideoBackendBase.cpp",
  "Source/Core/VideoCommon/VideoBackendBase.h",
  "Source/Core/VideoCommon/VideoState.cpp",
  "Source/Core/VideoCommon/VideoState.h",
  "Source/Core/Common/Config/ConfigInfo.h",
  "Source/Core/Common/Config/Config.h",
  "Source/Core/Common/Config/Layer.h",
  "Source/Core/Common/Config/Layer.cpp",
  "Source/Core/Common/Config/Config.cpp",
  "Source/Core/Common/ChunkFile.h",
  "Source/Core/Common/x64CPUDetect.cpp",
  "Source/Core/VideoCommon/TextureCacheBase.cpp",
  "Source/Core/VideoCommon/TextureCacheBase.h",
  "Source/Core/VideoCommon/TextureConfig.cpp",
  "Source/Core/VideoCommon/TextureConfig.h",
  "Source/Core/VideoCommon/AbstractStagingTexture.cpp",
  "Source/Core/VideoCommon/AbstractStagingTexture.h",
  "Source/Core/DolphinQt/Config/SDLHints/SDLHintsWindow.cpp",
  "Source/Core/Core/CMakeLists.txt",
  "Source/Core/Core/DSPEmulator.h",
  "Source/Core/Core/HW/DSPLLE/DSPLLE.cpp",
  "Source/Core/VideoCommon/Fifo.cpp",
  "Source/Core/VideoCommon/Fifo.h",
  "Source/Core/Core/HW/DSPLLE/DSPLLE.h",
  "Source/Core/Core/Core.cpp",
  "Source/Core/Core/Core.h",
  "Source/Core/Core/CoreTiming.cpp",
  "Source/Core/Core/AchievementManager.cpp",
  "Source/Core/DolphinQt/HotkeyScheduler.cpp",
  "Source/Core/Core/CoreTiming.h",
  "Source/Core/Core/Movie.cpp",
  "Source/Core/Core/Movie.h",
  "Source/Core/Core/HW/CPU.cpp",
  "Source/Core/Core/HW/EXI/EXI.h",
  "Source/Core/Core/HW/EXI/EXI.cpp",
  "Source/Core/Core/HW/EXI/EXI_Channel.cpp",
  "Source/Core/Core/HW/CPU.h",
  "Source/Core/Core/HW/GCPad.cpp",
  "Source/Core/Core/HW/ProcessorInterface.cpp",
  "Source/Core/Core/HW/ProcessorInterface.h",
  "Source/Core/Core/HW/WiimoteEmu/WiimoteEmu.cpp",
  "Source/Core/Core/PowerPC/BreakPoints.cpp",
  "Source/Core/Core/PowerPC/BreakPoints.h",
  "Source/Core/Core/PowerPC/PowerPC.cpp",
  "Source/Core/Core/PowerPC/PPCAnalyst.cpp",
  "Source/Core/Core/State.cpp",
  "Source/Core/Core/IOS/USB/Bluetooth/BTBase.h",
  "Source/Core/Core/State.h",
  "Source/Core/VideoCommon/FrameDumper.cpp",
  "Source/Core/VideoCommon/FrameDumper.h",
  "Source/Core/DolphinNoGUI/Platform.cpp",
  "Source/Core/DolphinNoGUI/Platform.h",
  "Source/Core/DolphinNoGUI/PlatformHeadless.cpp",
  "Source/Core/DolphinNoGUI/MainNoGUI.cpp",
  "Source/Core/DolphinQt/Settings.cpp",
  "Source/Core/DolphinLib.props"
)
git -C $Src checkout -- $owned
if ($LASTEXITCODE -ne 0) { throw "failed to restore files owned by the patch stack" }
git -C $Src clean -fdq -- Source/Core/Core/EmuCap.cpp Source/Core/Core/EmuCap.h `
  Source/Core/Core/EmuCapInput.cpp Source/Core/Core/EmuCapInput.h Source/Core/Core/EmuCapTemporal.h Source/Core/Core/EmuCapOwner.h Source/Core/Core/EmuCapWire.h Source/Core/Core/EmuCapOwned.inl Source/Core/Core/EmuCapPacing.h Source/Core/Core/EmuCapAudio.h
if ($LASTEXITCODE -ne 0) { throw "failed to clean stale adapter sources" }

Copy-Item -LiteralPath (Join-Path $here "EmuCap.cpp") `
  -Destination (Join-Path $Src "Source\Core\Core\EmuCap.cpp") -Force
Copy-Item -LiteralPath (Join-Path $here "EmuCap.h") `
  -Destination (Join-Path $Src "Source\Core\Core\EmuCap.h") -Force
Copy-Item -LiteralPath (Join-Path $here "EmuCapInput.cpp") `
  -Destination (Join-Path $Src "Source\Core\Core\EmuCapInput.cpp") -Force
Copy-Item -LiteralPath (Join-Path $here "EmuCapInput.h") `
  -Destination (Join-Path $Src "Source\Core\Core\EmuCapInput.h") -Force
Copy-Item -LiteralPath (Join-Path $here "EmuCapTemporal.h") `
  -Destination (Join-Path $Src "Source\Core\Core\EmuCapTemporal.h") -Force
foreach ($source in @("EmuCapOwner.h", "EmuCapWire.h", "EmuCapOwned.inl")) {
  Copy-Item -LiteralPath (Join-Path $here $source) -Destination (Join-Path $Src "Source\Core\Core\$source") -Force
}
Copy-Item -LiteralPath (Join-Path $here "EmuCapPacing.h") `
  -Destination (Join-Path $Src "Source\Core\Core\EmuCapPacing.h") -Force

Copy-Item -LiteralPath (Join-Path $here "EmuCapAudio.h") `
  -Destination (Join-Path $Src "Source\Core\Core\EmuCapAudio.h") -Force

# Match the bytewise patch order used by the Unix recipe and lock manifest.
[string[]]$patchNames = @(Get-ChildItem -LiteralPath (Join-Path $here "patches") -Filter "*.patch" | ForEach-Object { $_.Name })
[Array]::Sort($patchNames, [StringComparer]::Ordinal)
foreach ($patchName in $patchNames) {
  $patch = Get-Item -LiteralPath (Join-Path (Join-Path $here "patches") $patchName)
  Write-Output "[patch] applying $($patch.Name)"
  git -C $Src apply --check $patch.FullName
  if ($LASTEXITCODE -ne 0) { throw "patch does not apply cleanly: $($patch.Name)" }
  git -C $Src apply $patch.FullName
  if ($LASTEXITCODE -ne 0) { throw "failed to apply patch: $($patch.Name)" }
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
  throw "Visual Studio Installer vswhere.exe was not found"
}
$installation = & $vswhere -latest -products "*" -version '[17.0,18.0)' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if ($LASTEXITCODE -ne 0 -or -not $installation) { throw "Visual Studio 2022 C++ tools were not found" }
$vcvars = Join-Path $installation "VC\Auxiliary\Build\vcvars64.bat"
if (-not (Test-Path -LiteralPath $vcvars -PathType Leaf)) { throw "Visual Studio vcvars64.bat was not found" }

$bat = @"
@echo off
call "$vcvars"
if errorlevel 1 exit /b %ERRORLEVEL%
cd /d "$Src"
msbuild Source\dolphin-emu.sln /p:Configuration=Release /p:Platform=x64 /m:$Jobs /p:CL_MPCount=1 /v:minimal /nologo
if errorlevel 1 exit /b %ERRORLEVEL%
rem Upstream excludes DolphinNoGUI from the default solution configuration.
msbuild Source\Core\DolphinNoGUI\DolphinNoGUI.vcxproj /p:Configuration=Release /p:Platform=x64 /m:$Jobs /p:CL_MPCount=1 /v:minimal /nologo
exit /b %ERRORLEVEL%
"@
$tmp = Join-Path $env:TEMP "emucap-dolphin-build-$PID.bat"
try {
  [System.IO.File]::WriteAllText($tmp, $bat, [System.Text.Encoding]::ASCII)
  & cmd /c $tmp
  if ($LASTEXITCODE -ne 0) { throw "Dolphin MSBuild failed with exit code $LASTEXITCODE" }
} finally {
  Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
}

function Get-LowerSha256([string]$Path) {
  (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

$digestInputs = @("EmuCap.cpp", "EmuCap.h", "EmuCapInput.cpp", "EmuCapInput.h", "EmuCapTemporal.h", "EmuCapPacing.h", "EmuCapAudio.h", "EmuCapOwner.h", "EmuCapWire.h", "EmuCapOwned.inl")
$digestInputs += $patchNames | ForEach-Object { "patches/$_" }
$manifestLines = foreach ($relative in $digestInputs) {
  $nativePath = Join-Path $here ($relative -replace '/', [System.IO.Path]::DirectorySeparatorChar)
  "$(Get-LowerSha256 $nativePath)  $relative"
}
$manifestBytes = [System.Text.UTF8Encoding]::new($false).GetBytes(
  (($manifestLines -join "`n") + "`n")
)
$sha = [System.Security.Cryptography.SHA256]::Create()
try {
  $patchset = (
    [System.BitConverter]::ToString($sha.ComputeHash($manifestBytes)) -replace '-', ''
  ).ToLowerInvariant()
} finally {
  $sha.Dispose()
}
if ($lock.DOLPHIN_PATCHSET_SHA256 -ne "pending" -and
    $patchset -ne $lock.DOLPHIN_PATCHSET_SHA256) {
  throw "patchset digest differs from upstream.lock"
}

$binary = Join-Path $Src "Binary\x64\Dolphin.exe"
foreach ($name in @('Dolphin.exe', 'DolphinNoGUI.exe')) {
  $required = Join-Path $Src "Binary\x64\$name"
  if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
    throw "Dolphin build completed without the expected binary: $required"
  }
}
$metadata = [ordered]@{
  upstream = $lock.DOLPHIN_REPO
  commit = $lock.DOLPHIN_COMMIT
  host_api = [int]$lock.DOLPHIN_HOST_API
  patchset_sha256 = $patchset
} | ConvertTo-Json
$metadataPath = Join-Path (Split-Path -Parent $binary) "emucap-dolphin-build.json"
[System.IO.File]::WriteAllText(
  $metadataPath,
  "$metadata`n",
  [System.Text.UTF8Encoding]::new($false)
)
Write-Output "[build] completed: $binary"
Write-Output "[build] metadata: $metadataPath"
