$ErrorActionPreference = "Stop"

$scriptPath = $PSCommandPath
if (-not $scriptPath) {
    $scriptPath = $MyInvocation.MyCommand.Path
}

$toolsDir = Split-Path -Parent $scriptPath
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $toolsDir "..")).Path

function Resolve-EmucapBinary {
    param([Parameter(Mandatory = $true)][string]$Name)

    $candidates = @(
        (Join-Path $repoRoot "target\release\$Name.exe"),
        (Join-Path $repoRoot "target\release\$Name")
    )

    foreach ($path in $candidates) {
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            return (Resolve-Path -LiteralPath $path).Path
        }
    }

    throw "Missing $Name. Run cargo build --release --bin emucap --bin emucap-mcp --bin emucap-track-mcp --bin emucap-broker --bin emucap-mame-pc98-bridge."
}

$codexCli = if ($env:CODEX_CLI) { $env:CODEX_CLI } else { "codex" }
if ($env:EMUCAP_REGISTER_DRY_RUN -ne "1" -and -not (Get-Command $codexCli -ErrorAction SilentlyContinue)) {
    throw "Codex CLI not found. Install Codex or set CODEX_CLI to the full path of codex."
}

$controlMcp = Resolve-EmucapBinary "emucap-mcp"
$trackMcp = Resolve-EmucapBinary "emucap-track-mcp"

function Write-RegistrationMessage {
    param([Parameter(Mandatory = $true)][string]$Message)

    if ($env:EMUCAP_REGISTER_DRY_RUN -eq "1") {
        Write-Output $Message
    } else {
        Write-Host $Message
    }
}

if ($env:EMUCAP_REGISTER_DRY_RUN -eq "1") {
    Write-RegistrationMessage "Dry run: would register Codex MCP servers:"
} else {
    & $codexCli mcp add emucap --env "EMUCAP_REPO_ROOT=$repoRoot" -- $controlMcp
    & $codexCli mcp add emucap-track -- $trackMcp
    Write-RegistrationMessage "Registered Codex MCP servers:"
}

Write-RegistrationMessage "  emucap       -> $controlMcp"
Write-RegistrationMessage "  emucap-track -> $trackMcp"
if ($env:EMUCAP_REGISTER_DRY_RUN -ne "1") {
    Write-RegistrationMessage "Reconnect the agent session so the new MCP tool list is loaded."
}
