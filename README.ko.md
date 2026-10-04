# emucap

**AI 에이전트가 에뮬레이터 속 게임을 보고, 조작하고, 디버깅하도록 연결합니다.**

[English](README.md) · [릴리스](https://github.com/mcpads/emucap/releases) · [에이전트 가이드](AGENT_GUIDE.ko.md)

emucap은 Claude Code, Codex 같은 MCP 지원 에이전트를 레트로 게임 에뮬레이터에
연결합니다. 에이전트에게 특정 장면을 플레이하거나, 버그를 재현하거나, 게임 내부의
동작을 조사하도록 요청할 수 있습니다. 에이전트는 여러 에뮬레이터에서 같은 인터페이스로
화면을 보고, 버튼을 누르고, 프레임을 진행하고, 메모리를 읽습니다.

## 할 수 있는 일

- **플레이와 관찰:** 화면 캡처, 버튼 누르기와 길게 유지하기, 마우스 조작,
  지원되는 런타임의 배속 조절.
- **문제 재현:** 체크포인트 저장, 입력 재생, 결과 비교.
- **게임 내부 조사:** 메모리와 레지스터 읽기, 중단점 설정, 명령어 단위 실행.
- **실험 기록:** 별도의 Tracking MCP로 실행 결과와 발견 사항을 기록하고 비교.

게임 경로와 함께 에이전트에게 이렇게 요청할 수 있습니다.

> “이 게임을 열고 시작 전에 멈춰줘.”
>
> “체크포인트를 저장하고, 오른쪽을 60프레임 누른 뒤 화면을 보여줘.”
>
> “플레이어가 피해를 받을 때 이 메모리 주소를 바꾸는 코드를 찾아줘.”

제공되는 기능은 에뮬레이터와 시스템에 따라 다릅니다. 연결된 런타임이 `status`로
가용 기능을 알려주므로 에이전트가 그에 맞춰 조작합니다.

## 시작하기

에이전트에게 이 저장소와 사용할 게임을 알려주세요.

> “AGENT_GUIDE.ko.md에 따라 emucap을 설치하고 MCP 서버로 등록해줘.
> 이 게임에 필요한 어댑터도 준비해줘.”

에이전트가 코어를 설치하고, 선택한 에뮬레이터의 어댑터를 준비하고, 연결을 확인합니다.
게임과 필요한 BIOS·펌웨어는 사용자가 준비합니다.

[GitHub Releases](https://github.com/mcpads/emucap/releases)에서 **Windows x86-64,
Linux x86-64, macOS Apple Silicon**용 코어 패키지를 제공합니다.
Intel macOS는 소스 빌드로 사용할 수 있습니다. 에뮬레이터 어댑터는 별도로 준비하며,
운영체제에 따라 빌드 요구사항과 제공 기능이 다릅니다.

직접 설치하려면 [설치 절차](AGENT_GUIDE.ko.md#1-core-설치-방식-선택)를 참고하세요.
코어는 에뮬레이터를 조작하는 **Control**과 실험을 기록하는 **Tracking**, 두 MCP 서버를
제공합니다. Tracking은 선택 사항이며 에이전트가 필요에 따라 함께 사용합니다.

## 지원 시스템

| 에뮬레이터 | 시스템 |
| --- | --- |
| Mesen2 | NES, SNES, Master System, Game Gear, Game Boy / Color, GBA |
| Mednafen | PlayStation, Saturn, PC Engine, PC-FX, Mega Drive / Genesis, WonderSwan / Color, Neo Geo Pocket / Color |
| Flycast | Dreamcast |
| DeSmuME | Nintendo DS |
| PPSSPP | PSP |
| PCSX2 | PlayStation 2 |
| Dolphin | GameCube, Wii |
| MAME / NP2kai | PC-98 |
| MAME | Neo Geo MVS / AES / CD |
| Mupen64Plus | Nintendo 64 — 실험 지원 |
| openMSX | MSX1 / MSX2 / MSX2+ |
| xemu | 원본 Xbox — 실험 지원 |

설치 방법과 프로필별 지원 범위는 [어댑터 가이드](AGENT_GUIDE.ko.md#에뮬레이터별-어댑터-필요할-때-에이전트가-설치)를
참고하세요. 제어와 관찰에 에뮬레이터 내부 수정이 필요한 경우 emucap이 소스 패치를 유지합니다.

## 프로젝트 상태

**1.0.0이 출시되었습니다.** 1.x의 공개 호환성 계약은
[COMPATIBILITY.md](COMPATIBILITY.md), 변경 사항은 [CHANGELOG.md](CHANGELOG.md)에 있습니다.

코어와 별도 표시가 없는 소스의 라이선스는 **GPL-2.0-or-later**입니다.
에뮬레이터 패치는 각 원본 프로젝트의 라이선스를 따릅니다.
[LICENSE](LICENSE)와 [NOTICE](NOTICE)를 참고하세요.
