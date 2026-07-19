# emucap ↔ Dolphin(GameCube/Wii) 네이티브 포크 런치 헬퍼.
#
#   launch-native.ps1 <ISO> <EMUCAP_PORT> [NAME] [-Dolphin <forked exe>] [-User <dir>]
#
# GDB 브리지와 달리 포크된 Dolphin에 emucap 서버가 임베드돼 있다(Core/EmuCap.cpp). 이 스크립트는
# 세션 토큰/포트/콘텐츠를 환경변수로 넘겨 포크 Dolphin을 띄우기만 하면 된다 — 별도 브리지 프로세스도,
# GDB 스텁도, CachedInterpreter 강제도 없다(JIT 사용 가능 = 빠름). savestate/screenshot 포함 풀 제어.
param(
  [Parameter(Mandatory=$true)][string]$Iso,
  [Parameter(Mandatory=$true)][int]$EmucapPort,
  [string]$Name = "dolphin",
  [string]$Dolphin = "C:\dolphin-build\dolphin-src\Binary\x64\Dolphin.exe",
  [string]$User = "$env:LOCALAPPDATA\emucap\dolphin\user",
  # exec breakpoint 는 JIT 에서 신뢰성이 떨어진다(이미 컴파일된 블록엔 체크가 없음).
  # BP 를 쓸 작업이면 -Interpreter 로 CachedInterpreter(=5)를 강제한다.
  [switch]$Interpreter
)
$ErrorActionPreference = "Stop"

# -Interpreter: 순수 Interpreter(CPUCore=0) + 디버깅 모드. exec BP 신뢰성을 위해 CachedInterpreter(5)
# 가 아니라 순수 Interpreter(0)를 쓴다 — Interpreter.cpp 는 매 명령마다 런타임에
# `if (Config::IsDebuggingEnabled()) CheckAndHandleBreakPoints()` 를 실행하므로(컴파일 게이트·블록
# 재컴파일 타이밍 무관) BP 가 확실히 히트한다. CachedInterpreter(5)는 컴파일 시점에만 CheckBreakpoint
# 를 삽입해 이미 컴파일된 블록엔 체크가 없어 히트하지 않는 문제가 있었다. 느리지만 set_input 으로
# 컷신을 건너뛰면 되고, 덤프엔 정확성이 속도보다 중요하다.
$cpuCore = if ($Interpreter) { "CPUCore = 0`n" } else { "" }
$dbg = if ($Interpreter) { "DebugModeEnabled = True`n" } else { "" }
New-Item -ItemType Directory -Force -Path "$User\Config" | Out-Null
@"
[Core]
$cpuCore[Interface]
${dbg}ConfirmStop = False
UsePanicHandlers = False
[DSP]
Backend = No Audio Output
Volume = 0
[Analytics]
Enabled = False
PermissionAsked = True
"@ | Out-File -Encoding ascii "$User\Config\Dolphin.ini"

# 포크 Dolphin의 EmuCap::Start 가 읽는 환경변수. 세션 토큰은 identity_guard 통과용(포트별 파일).
$tokenFile = "C:\emutmp\emucap_session_token_$EmucapPort"
$env:EMUCAP_PORT = "$EmucapPort"
$env:EMUCAP_SESSION_TOKEN = if (Test-Path $tokenFile) { (Get-Content $tokenFile -Raw).Trim() } else { "" }
$env:EMUCAP_NAME = $Name
$env:EMUCAP_CONTENT = $Iso

# 경로는 반드시 인용(공백 시 잘려 부팅 실패).
$p = Start-Process -FilePath $Dolphin `
  -ArgumentList @("--user", "`"$User`"", "--exec", "`"$Iso`"", "--batch") -PassThru
$p.Id | Out-File -Encoding ascii "$User\dolphin.pid"
Write-Output "[launch-native] Dolphin(fork) pid=$($p.Id) → emucap 127.0.0.1:$EmucapPort. emucap status 로 확인."
