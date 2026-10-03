# Install a pinned software OpenGL package beside explicitly selected validation executables.
# The plan is private input: {asset:{url,digest},directories:["native/xemu", ...]}.
param(
  [Parameter(Mandatory=$true)][string]$Root,
  [Parameter(Mandatory=$true)][string]$Plan,
  [Parameter(Mandatory=$true)][string]$SevenZip
)
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Run on the Windows validation host' }
$rootPath = (Resolve-Path -LiteralPath $Root).Path.TrimEnd('\')
$inputPlan = Get-Content -LiteralPath $Plan -Raw | ConvertFrom-Json
if ($inputPlan.asset.digest -notmatch '^sha256:([0-9a-f]{64})$') { throw 'Missing pinned archive digest' }
$expected = $Matches[1]
if (-not $inputPlan.directories.Count) { throw 'No selected application directories' }
$targets = foreach ($relative in $inputPlan.directories) {
  $target = (Resolve-Path -LiteralPath (Join-Path $rootPath $relative)).Path
  if (-not $target.StartsWith($rootPath + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw "Application directory escapes validation root: $relative"
  }
  if (-not (Get-ChildItem -LiteralPath $target -Filter '*.exe' -File)) {
    throw "No executable in application directory: $target"
  }
  $target
}
$cache = Join-Path $rootPath ('software-gl/' + $expected)
New-Item -ItemType Directory -Path $cache -Force | Out-Null
$archive = Join-Path $cache 'mesa.7z'
if (-not (Test-Path -LiteralPath $archive)) {
  Invoke-WebRequest -Uri $inputPlan.asset.url -OutFile $archive
}
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
  throw 'Software OpenGL archive digest mismatch'
}
$unpacked = Join-Path $cache 'unpacked'
& $SevenZip x -y "-o$unpacked" $archive | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Software OpenGL extraction failed' }
$files = @('opengl32.dll', 'libgallium_wgl.dll')
if (Test-Path -LiteralPath (Join-Path $unpacked 'x64/libglapi.dll')) { $files += 'libglapi.dll' }
$copies = foreach ($name in $files) {
  $source = Join-Path $unpacked ('x64/' + $name)
  $digest = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()
  foreach ($target in $targets) {
    $destination = Join-Path $target $name
    if ((Test-Path -LiteralPath $destination) -and
        (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant() -ne $digest) {
      throw "Existing application DLL differs: $destination"
    }
    [PSCustomObject]@{source=$source; path=$destination; sha256=$digest}
  }
}
foreach ($copy in $copies) {
  Copy-Item -LiteralPath $copy.source -Destination $copy.path
  if ((Get-FileHash -LiteralPath $copy.path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $copy.sha256) {
    throw "Copied application DLL differs: $($copy.path)"
  }
}
[PSCustomObject]@{archive_sha256=$expected; files=$copies; runtime_validated=$false} |
  ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $cache 'deployment.json') -Encoding UTF8
