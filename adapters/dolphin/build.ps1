# emucap Dolphin(GameCube/Wii) 네이티브 어댑터 빌드.
#
# Dolphin 소스를 클론(핀 커밋)해 emucap 서버(EmuCap.cpp/h)를 심고 3개 파일을 패치한 뒤
# Visual Studio 솔루션을 MSBuild 로 빌드한다. 산출물: <src>\Binary\x64\Dolphin.exe.
#
# 전제: Visual Studio 2022(또는 Build Tools, 최신 MSVC + Windows SDK), Git. Qt 는 Dolphin
# 이 Externals 서브모듈(Qt6.8.3 prebuilt)로 가져오므로 별도 설치 불필요.
#
#   build.ps1 [-Src C:\dolphin-build\dolphin-src] [-Jobs N]
param(
  [string]$Src = "C:\dolphin-build\dolphin-src",
  [string]$DolphinCommit = "415ec4de182034179143231e84c17dbcdf8be8aa"
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

# 1) 소스 확보(얕은 클론 + 서브모듈). 이미 있으면 재사용.
if (-not (Test-Path "$Src\Source\dolphin-emu.sln")) {
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Src) | Out-Null
  git clone --recurse-submodules --shallow-submodules --jobs 4 `
    https://github.com/dolphin-emu/dolphin.git $Src
  git -C $Src checkout $DolphinCommit
  git -C $Src submodule update --init --recursive --depth 1
}

# 2) emucap 서버 소스를 Core 에 배치.
Copy-Item "$here\EmuCap.cpp" "$Src\Source\Core\Core\EmuCap.cpp" -Force
Copy-Item "$here\EmuCap.h"   "$Src\Source\Core\Core\EmuCap.h"   -Force

# 3) 3개 파일 패치(멱등: 이미 적용됐으면 건너뜀).
#    - DolphinLib.props : EmuCap.cpp/h 를 프로젝트에 등록
#    - Core/Core.cpp    : EmuThread 에서 EmuCap::Start/Stop 훅
#    - Core/HW/GCPad.cpp: GetStatus 폴 지점에 입력 오버라이드 훅
foreach ($p in @("DolphinLib.props", "Core.cpp", "GCPad.cpp")) {
  $patch = "$here\patches\$p.patch"
  # --forward 는 이미 적용된 패치를 조용히 건너뛴다. Git bash 의 git apply 사용.
  git -C $Src apply --3way --whitespace=nowarn $patch 2>$null
  if ($LASTEXITCODE -ne 0) {
    git -C $Src apply --reverse --check $patch 2>$null
    if ($LASTEXITCODE -eq 0) { Write-Output "[patch] $p already applied" }
    else { throw "patch 적용 실패: $p (Dolphin 소스 버전이 핀 커밋과 다를 수 있음)" }
  } else { Write-Output "[patch] applied $p" }
}

# 4) 빌드(vcvars64 + MSBuild Release x64).
$vcvars = "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
if (-not (Test-Path $vcvars)) { throw "vcvars64.bat 없음 — VS2022 설치 경로 확인" }
$bat = @"
@echo off
call "$vcvars"
cd /d "$Src"
msbuild Source\dolphin-emu.sln /p:Configuration=Release /p:Platform=x64 /m /v:minimal /nologo
echo BUILD_EXIT_CODE=%ERRORLEVEL%
"@
$tmp = Join-Path $env:TEMP "emucap_dolphin_build.bat"
$bat | Out-File -Encoding ascii $tmp
& cmd /c $tmp
Write-Output "[build] 완료 → $Src\Binary\x64\Dolphin.exe"
