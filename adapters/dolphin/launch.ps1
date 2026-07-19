# emucap ↔ Dolphin(GameCube/Wii) 런치 헬퍼.
#
#   launch.ps1 <ISO> <EMUCAP_PORT> [NAME] [-GdbPort <n>] [-Dolphin <exe>] [-User <dir>]
#
# 하는 일: emucap 소유 user 디렉터리에 GDB 스텁을 켠 config를 심고, Dolphin을 --batch
# --exec 로 띄운 뒤(경로는 공백 안전하게 인용), GDB 포트가 열리길 기다렸다가
# emucap-gdb-bridge.py 를 세션 토큰과 함께 백그라운드로 붙인다.
#
# GDB 스텁은 단발(첫 클라이언트 분리 시 리스너를 닫음)이므로 브리지가 유일한 지속
# 클라이언트다 — 별도로 GDB에 접속하지 말 것. JIT는 GDB 미지원이라 CachedInterpreter를 쓴다.
param(
  [Parameter(Mandatory=$true)][string]$Iso,
  [Parameter(Mandatory=$true)][int]$EmucapPort,
  [string]$Name = "dolphin",
  [int]$GdbPort = 2159,
  [string]$Dolphin = "D:\BH4\tools\dolphin\Dolphin-x64\Dolphin.exe",
  [string]$User = "$env:LOCALAPPDATA\emucap\dolphin\user"
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

New-Item -ItemType Directory -Force -Path "$User\Config" | Out-Null
@"
[General]
GDBPort = $GdbPort
[Core]
CPUCore = 5
CPUThread = False
[Interface]
ConfirmStop = False
UsePanicHandlers = False
OnScreenDisplayMessages = False
[DSP]
Backend = No Audio Output
Volume = 0
[Analytics]
Enabled = False
PermissionAsked = True
"@ | Out-File -Encoding ascii "$User\Config\Dolphin.ini"

# 이미 뜬 Dolphin은 건드리지 않는다(다중 세션 안전). 이 스크립트가 띄운 것만 pid로 추적.
$p = Start-Process -FilePath $Dolphin `
  -ArgumentList @("--user", "`"$User`"", "--exec", "`"$Iso`"", "--batch") -PassThru
$p.Id | Out-File -Encoding ascii "$User\dolphin.pid"
Write-Output "[launch] Dolphin pid=$($p.Id) (GDB $GdbPort)"

# GDB 포트가 열릴 때까지 대기(부팅 시간).
$deadline = (Get-Date).AddSeconds(40)
while ((Get-Date) -lt $deadline) {
  if (Get-NetTCPConnection -LocalPort $GdbPort -State Listen -ErrorAction SilentlyContinue) { break }
  Start-Sleep -Milliseconds 500
}
if (-not (Get-NetTCPConnection -LocalPort $GdbPort -State Listen -ErrorAction SilentlyContinue)) {
  throw "GDB 스텁 포트 $GdbPort 가 열리지 않음 (Dolphin 부팅 실패? ISO 경로 확인)"
}
Write-Output "[launch] GDB 스텁 준비됨."

# emucap 세션 토큰(포트별)을 읽어 브리지 환경에 전달 — identity_guard 통과용.
$tokenFile = "C:\emutmp\emucap_session_token_$EmucapPort"
$env:EMUCAP_SESSION_TOKEN = if (Test-Path $tokenFile) { (Get-Content $tokenFile -Raw).Trim() } else { "" }
$env:EMUCAP_NAME = $Name
$env:EMUCAP_CONTENT = $Iso

$bridge = Join-Path $here "emucap-gdb-bridge.py"
$b = Start-Process -FilePath "python" `
  -ArgumentList @("`"$bridge`"", "$EmucapPort", "127.0.0.1:$GdbPort") `
  -PassThru -WindowStyle Hidden
$b.Id | Out-File -Encoding ascii "$User\bridge.pid"
Write-Output "[launch] 브리지 pid=$($b.Id) → emucap 127.0.0.1:$EmucapPort. 이제 emucap status로 확인."
