use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "emucap", about = "에뮬레이터 캡처 케이스 번들 도구")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// 어댑터가 떨군 raw 번들을 검증된 manifest.json으로 확정한다.
    Finalize {
        dir: PathBuf,
        /// _raw.json의 rom_path 대신 이 ROM 경로로 SHA-1을 계산한다.
        #[arg(long)]
        rom: Option<PathBuf>,
    },
    /// 번들 요약을 출력한다.
    Inspect {
        dir: PathBuf,
        /// 사람용 표 대신 JSON으로 출력
        #[arg(long)]
        json: bool,
    },
    /// 두 리전 덤프 디렉토리의 메모리를 비교해 최초 분기점을 보고한다.
    Diff {
        dir_a: PathBuf,
        dir_b: PathBuf,
        /// 비교 제외 범위(여러 번 가능): region:start-end (예: wram:256-512)
        #[arg(long = "ignore")]
        ignore: Vec<String>,
        /// 기준선 diff JSON(정상 지점의 diff): 여기 든 분기 오프셋을 제외(예상 차이 빼기)
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// 상태(state.json) 디프에서 추가로 제외할 키 부분문자열(여러 번 가능)
        #[arg(long = "ignore-key")]
        ignore_key: Vec<String>,
        /// 사람용 표 대신 JSON으로 출력
        #[arg(long)]
        json: bool,
    },
    /// 확정 버그를 회귀 케이스로 등록한다.
    Regression {
        #[command(subcommand)]
        action: RegressionAction,
    },
    /// 실험 추적 원장을 다룬다(reindex/import/ls/show/compare/summarize).
    Track {
        #[command(subcommand)]
        action: TrackAction,
    },
}

#[derive(Subcommand)]
pub enum TrackAction {
    /// FS 정본을 walk해 index.sqlite를 재생성한다.
    Reindex,
    /// 기존 번들(manifest.json)을 run으로 흡수한다.
    Import { bundle: PathBuf },
    /// run 목록을 출력한다.
    Ls {
        #[arg(long)]
        rom: Option<String>,
        #[arg(long)]
        goal: Option<String>,
    },
    /// run 상세(run.json)를 출력한다.
    Show {
        #[arg(long)]
        rom: String,
        run_id: String,
    },
    /// 두 run을 비교한다(메트릭·게이트·재현성·개입·산출물).
    Compare { run_id_a: String, run_id_b: String },
    /// run들을 goal/tag/rom로 묶어 횡단 요약한다.
    Summarize {
        #[arg(long)]
        goal: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        rom: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RegressionAction {
    /// 케이스를 스위트에 추가한다.
    Add {
        suite_dir: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        desc: String,
        /// savestate 케이스: 이 .mss를 복사
        #[arg(long)]
        from_savestate: Option<PathBuf>,
        /// savestate 진행 프레임
        #[arg(long, default_value_t = 0)]
        advance: u64,
        /// input_replay 케이스: 이 무비를 복사
        #[arg(long)]
        from_input: Option<PathBuf>,
        /// input_replay 시작점(reset 또는 베이스 .mss)
        #[arg(long)]
        start: Option<PathBuf>,
        /// input_replay 앵커 술어: memory_type:address:length:op:value (savestate 케이스는 무시됨)
        #[arg(long)]
        anchor: Option<String>,
        /// 판정 술어: memory_type:address:length:op:value
        #[arg(long)]
        predicate: String,
        /// 케이스 ROM SHA-1 계산용 원본 경로
        #[arg(long)]
        rom: PathBuf,
        #[arg(long, default_value = "absent")]
        expect: String,
    },
}
