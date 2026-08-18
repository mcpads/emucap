# Build the pinned Dolphin native adapter on Windows.
#
# Requirements: Visual Studio 2022 with MSVC and a Windows SDK, plus Git.
#
#   build.ps1 [-Src C:\dolphin-build\dolphin-src]
param(
  [string]$Src = "C:\dolphin-build\dolphin-src"
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

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
  "Source/Core/Core/CMakeLists.txt",
  "Source/Core/Core/Core.cpp",
  "Source/Core/Core/Core.h",
  "Source/Core/Core/HW/CPU.cpp",
  "Source/Core/Core/HW/CPU.h",
  "Source/Core/Core/HW/GCPad.cpp",
  "Source/Core/Core/HW/ProcessorInterface.cpp",
  "Source/Core/Core/HW/ProcessorInterface.h",
  "Source/Core/Core/HW/WiimoteEmu/WiimoteEmu.cpp",
  "Source/Core/Core/PowerPC/BreakPoints.cpp",
  "Source/Core/Core/PowerPC/BreakPoints.h",
  "Source/Core/Core/PowerPC/PowerPC.cpp",
  "Source/Core/Core/State.cpp",
  "Source/Core/Core/State.h",
  "Source/Core/VideoCommon/FrameDumper.cpp",
  "Source/Core/VideoCommon/FrameDumper.h",
  "Source/Core/DolphinQt/Settings.cpp",
  "Source/Core/DolphinLib.props"
)
git -C $Src checkout -- $owned
if ($LASTEXITCODE -ne 0) { throw "failed to restore files owned by the patch stack" }
git -C $Src clean -fdq -- Source/Core/Core/EmuCap.cpp Source/Core/Core/EmuCap.h `
  Source/Core/Core/EmuCapInput.cpp Source/Core/Core/EmuCapInput.h
if ($LASTEXITCODE -ne 0) { throw "failed to clean stale adapter sources" }

Copy-Item -LiteralPath (Join-Path $here "EmuCap.cpp") `
  -Destination (Join-Path $Src "Source\Core\Core\EmuCap.cpp") -Force
Copy-Item -LiteralPath (Join-Path $here "EmuCap.h") `
  -Destination (Join-Path $Src "Source\Core\Core\EmuCap.h") -Force
Copy-Item -LiteralPath (Join-Path $here "EmuCapInput.cpp") `
  -Destination (Join-Path $Src "Source\Core\Core\EmuCapInput.cpp") -Force
Copy-Item -LiteralPath (Join-Path $here "EmuCapInput.h") `
  -Destination (Join-Path $Src "Source\Core\Core\EmuCapInput.h") -Force

foreach ($patch in Get-ChildItem -LiteralPath (Join-Path $here "patches") -Filter "*.patch" |
    Sort-Object Name) {
  Write-Output "[patch] applying $($patch.Name)"
  git -C $Src apply --check $patch.FullName
  if ($LASTEXITCODE -ne 0) { throw "patch does not apply cleanly: $($patch.Name)" }
  git -C $Src apply $patch.FullName
  if ($LASTEXITCODE -ne 0) { throw "failed to apply patch: $($patch.Name)" }
}

$vcvarsCandidates = @(
  "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat",
  "C:\Program Files\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat",
  "C:\Program Files\Microsoft Visual Studio\2022\Professional\VC\Auxiliary\Build\vcvars64.bat",
  "C:\Program Files\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvars64.bat"
)
$vcvars = $vcvarsCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
  Select-Object -First 1
if (-not $vcvars) { throw "Visual Studio 2022 vcvars64.bat was not found" }

$bat = @"
@echo off
call "$vcvars"
cd /d "$Src"
msbuild Source\dolphin-emu.sln /p:Configuration=Release /p:Platform=x64 /m /v:minimal /nologo
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

$digestInputs = @("EmuCap.cpp", "EmuCap.h", "EmuCapInput.cpp", "EmuCapInput.h")
$digestInputs += Get-ChildItem -LiteralPath (Join-Path $here "patches") -Filter "*.patch" |
  Sort-Object Name |
  ForEach-Object { "patches/$($_.Name)" }
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
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
  throw "Dolphin build completed without the expected binary: $binary"
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
