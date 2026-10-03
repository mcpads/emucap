# Called by build.sh while its source-tree lock remains held.
param(
  [Parameter(Mandatory=$true)][string]$Source,
  [Parameter(Mandatory=$true)][string]$Build,
  [Parameter(Mandatory=$true)][string]$MsysRoot,
  [int]$Jobs = 4
)
$ErrorActionPreference = "Stop"
if ($Jobs -lt 1) { throw "Jobs must be positive" }
if ($MsysRoot) {
  # MSVC dependencies must not discover MinGW pkg-config or CRT headers.
  $msysPrefix = $MsysRoot.Replace('/', '\').TrimEnd('\') + '\'
  $env:Path = (($env:Path -split ';') | Where-Object {
    -not $_.Replace('/', '\').StartsWith($msysPrefix, [StringComparison]::OrdinalIgnoreCase)
  }) -join ';'
  foreach ($key in @('PKG_CONFIG_PATH', 'PKG_CONFIG_LIBDIR', 'PKG_CONFIG_SYSROOT_DIR',
                     'CMAKE_PREFIX_PATH', 'CPATH', 'C_INCLUDE_PATH', 'CPLUS_INCLUDE_PATH', 'LIBRARY_PATH')) {
    [Environment]::SetEnvironmentVariable($key, $null, 'Process')
  }
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if ($LASTEXITCODE -ne 0 -or -not $installation) { throw "Visual Studio C++ tools are required" }
$vcvars = Join-Path $installation "VC/Auxiliary/Build/vcvars64.bat"
$environmentLines = & cmd.exe /d /s /c "call `"$vcvars`" >nul && set"
if ($LASTEXITCODE -ne 0) { throw "Visual Studio environment setup failed" }
foreach ($line in $environmentLines) {
  if ($line -match '^([^=]+)=(.*)$') {
    [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
  }
}
# This entrypoint may inherit MSYS2's PATH. Its CMake lacks the native MSVC
# resource dependency helper; use Visual Studio's Windows CMake for this build.
$cmakeDirectory = Join-Path $installation 'Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin'
$ninjaDirectory = Join-Path $installation 'Common7/IDE/CommonExtensions/Microsoft/CMake/Ninja'
if (-not (Test-Path (Join-Path $cmakeDirectory 'cmake.exe'))) {
  throw 'Visual Studio CMake tools are required'
}
if (-not (Test-Path (Join-Path $ninjaDirectory 'ninja.exe'))) {
  throw 'Visual Studio Ninja tools are required'
}
$env:Path = "$cmakeDirectory;$ninjaDirectory;$env:Path"
$dependencies = Join-Path $Source "deps"
$dependencyScript = Join-Path $Source ".github/workflows/scripts/windows/build-dependencies.bat"
$batchText = [IO.File]::ReadAllText($dependencyScript)
# cmd's batch-label scanning needs CRLF even though the checkout is LF-normalized.
[IO.File]::WriteAllText($dependencyScript, ($batchText -replace "\r?\n", "`r`n"), [Text.UTF8Encoding]::new($false))
$dependencyIdentity = ((Get-FileHash $dependencyScript -Algorithm SHA256).Hash + ':' +
  (Get-FileHash $PSCommandPath -Algorithm SHA256).Hash + ':' + $env:VCToolsVersion)
$dependencyReceipt = Join-Path $dependencies '.emucap-dependencies-complete'
if (-not (Test-Path $dependencyReceipt) -or
    [IO.File]::ReadAllText($dependencyReceipt).Trim() -ne $dependencyIdentity -or
    -not (Test-Path (Join-Path $dependencies "lib/cmake/Qt6/Qt6Config.cmake"))) {
  $env:EMUCAP_PCSX2_VCVARS = $vcvars
  $env:DEBUG = "0"
  $env:EMUCAP_BUILD_JOBS = [string]$Jobs
  & $dependencyScript
  if ($LASTEXITCODE -ne 0) { throw "PCSX2 dependency build failed" }
  [IO.File]::WriteAllText($dependencyReceipt, $dependencyIdentity, [Text.UTF8Encoding]::new($false))
}
& cmake -S $Source -B $Build -G Ninja -DCMAKE_BUILD_TYPE=Release `
  "-DCMAKE_PREFIX_PATH=$dependencies" -DCMAKE_C_COMPILER=cl -DCMAKE_CXX_COMPILER=cl `
  "-DCMAKE_RUNTIME_OUTPUT_DIRECTORY=$Build/bin" `
  "-DCMAKE_INSTALL_PREFIX=$Build/install" `
  -DENABLE_TESTS=OFF -DCMAKE_DISABLE_PRECOMPILE_HEADERS=ON -DDISABLE_ADVANCE_SIMD=ON
if ($LASTEXITCODE -ne 0) { throw "PCSX2 configuration failed" }
& cmake --build $Build --parallel $Jobs
if ($LASTEXITCODE -ne 0) { throw "PCSX2 build failed" }
# Upstream's install rules deploy its MSVC dependencies, Qt plugins and qt.conf
# into the source-owned bin directory. Keep the adapter's documented build/bin
# output as the complete runtime tree.
& cmake --install $Build --config Release --prefix "$Build/install"
if ($LASTEXITCODE -ne 0) { throw 'PCSX2 runtime installation failed' }
& cmake -E copy_directory "$Source/bin" "$Build/bin"
if ($LASTEXITCODE -ne 0) { throw 'PCSX2 runtime staging failed' }
