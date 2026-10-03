# Build the pinned PPSSPP Headless adapter with Visual Studio 2022 and CMake.
param([ValidateRange(1, 64)][int]$Jobs = 2)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$work = if ($env:EMUCAP_PPSSPP_WORK) { $env:EMUCAP_PPSSPP_WORK } else { Join-Path $here "work" }

function Invoke-Checked([string]$Program, [string[]]$Arguments) {
  & $Program @Arguments
  if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}

New-Item -ItemType Directory -Force -Path $work | Out-Null
$workItem = Get-Item -Force -LiteralPath $work
if (($workItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
  throw "PPSSPP work path must not be a reparse point: $work"
}
$work = $workItem.FullName
$marker = Join-Path $work ".emucap-ppsspp-work"
if ($env:EMUCAP_PPSSPP_WORK -and -not (Test-Path -LiteralPath $marker) -and
    @(Get-ChildItem -Force -LiteralPath $work).Count -gt 0) {
  throw "EMUCAP_PPSSPP_WORK is not empty or emucap-owned: $work"
}
$sha = [Security.Cryptography.SHA256]::Create()
try {
  $suffix = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($work.ToLowerInvariant()))).Replace('-', '')
} finally { $sha.Dispose() }
$mutex = [Threading.Mutex]::new($false, "Local\emucap-ppsspp-build-$suffix")
$held = $false
try {
  try { $held = $mutex.WaitOne() } catch [Threading.AbandonedMutexException] { $held = $true }
  New-Item -ItemType File -Force -Path $marker | Out-Null
  $lock = @{}
  Get-Content -LiteralPath (Join-Path $here "upstream.lock") | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') { $lock[$matches[1]] = $matches[2] }
  }
  foreach ($key in @("PPSSPP_REPO", "PPSSPP_COMMIT", "PPSSPP_PATCHSET_SHA256")) {
    if (-not $lock[$key]) { throw "upstream.lock is missing $key" }
  }
  $patches = @(Get-ChildItem -LiteralPath (Join-Path $here "patches") -Filter "*.patch" | Sort-Object Name)
  $stream = [IO.MemoryStream]::new()
  $sha = [Security.Cryptography.SHA256]::Create()
  try {
    foreach ($patch in $patches) {
      $bytes = [IO.File]::ReadAllBytes($patch.FullName)
      $stream.Write($bytes, 0, $bytes.Length)
    }
    $digest = [BitConverter]::ToString($sha.ComputeHash($stream.ToArray())).Replace('-', '').ToLowerInvariant()
  } finally { $stream.Dispose(); $sha.Dispose() }
  if ($digest -ne $lock.PPSSPP_PATCHSET_SHA256) { throw "PPSSPP patch stack differs from upstream.lock" }

  $src = Join-Path $work "ppsspp"
  if ((Test-Path -LiteralPath $src) -and
      ((Get-Item -Force -LiteralPath $src).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "PPSSPP source must not be a reparse point: $src"
  }
  if (-not (Test-Path -LiteralPath (Join-Path $src ".git"))) {
    $origin = if ($env:EMUCAP_PPSSPP_SRC) { $env:EMUCAP_PPSSPP_SRC } else { $lock.PPSSPP_REPO }
    if ((Test-Path -LiteralPath $src) -and @(Get-ChildItem -Force -LiteralPath $src).Count -gt 0) {
      throw "PPSSPP source exists but is not a git checkout: $src"
    }
    Invoke-Checked git @("init", $src)
    Invoke-Checked git @("-C", $src, "remote", "add", "origin", $origin)
  }
  Invoke-Checked git @("-C", $src, "fetch", "--depth", "1", "--update-shallow", "origin", $lock.PPSSPP_COMMIT)
  if (Test-Path -LiteralPath (Join-Path $src ".git/index")) {
    Invoke-Checked git @("-C", $src, "checkout", "--", ".")
    Invoke-Checked git @("-C", $src, "clean", "-fdq", "--", "Core", "headless")
  }
  Invoke-Checked git @("-C", $src, "checkout", "--detach", $lock.PPSSPP_COMMIT)
  Invoke-Checked git @("-C", $src, "submodule", "update", "--init", "--recursive", "--depth", "1", "--jobs", "$Jobs")
  foreach ($patch in $patches) {
    Invoke-Checked git @("-C", $src, "apply", "--check", $patch.FullName)
    Invoke-Checked git @("-C", $src, "apply", $patch.FullName)
  }

  $cmake = Get-Command cmake -ErrorAction SilentlyContinue
  if ($cmake) { $cmake = $cmake.Source } else {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere)) { throw "CMake and Visual Studio Installer were not found" }
    $installation = & $vswhere -latest -products "*" -version '[17.0,18.0)' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $installation) { throw "Visual Studio 2022 C++ tools were not found" }
    $cmake = Join-Path $installation "Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe"
    if (-not (Test-Path -LiteralPath $cmake)) { throw "Install the Visual Studio CMake component" }
  }
  $build = Join-Path $src "build-headless"
  Invoke-Checked $cmake @("-S", $src, "-B", $build, "-G", "Visual Studio 17 2022", "-A", "x64", "-DHEADLESS=ON")
  Invoke-Checked $cmake @("--build", $build, "--config", "Release", "--target", "PPSSPPHeadless", "--parallel", "$Jobs")
  $binDir = Join-Path $build "Release"
  $binary = Join-Path $binDir "PPSSPPHeadless.exe"
  if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { throw "PPSSPPHeadless.exe was not produced" }
  Copy-Item -LiteralPath (Join-Path $build "assets") -Destination $binDir -Recurse -Force
  $metadata = [ordered]@{
    upstream = $lock.PPSSPP_REPO
    commit = $lock.PPSSPP_COMMIT
    patchset_sha256 = $digest
    binary_sha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
  } | ConvertTo-Json
  [IO.File]::WriteAllText((Join-Path $binDir "emucap-ppsspp-build.json"), "$metadata`n", [Text.UTF8Encoding]::new($false))
  Write-Output "[build] completed: $binary"
} finally {
  if ($held) { $mutex.ReleaseMutex() }
  $mutex.Dispose()
}
