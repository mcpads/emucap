mod cli;
mod commands;

use clap::Parser;
use cli::{Cli, Command, RegressionAction, TrackAction};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Finalize { dir, rom } => commands::finalize::run(&dir, rom.as_deref()),
        Command::Inspect { dir, json } => commands::inspect::run(&dir, json),
        Command::Diff {
            dir_a,
            dir_b,
            ignore,
            baseline,
            ignore_key,
            json,
        } => commands::diff::run(
            &dir_a,
            &dir_b,
            &ignore,
            baseline.as_deref(),
            &ignore_key,
            json,
        ),
        Command::Regression { action } => match action {
            RegressionAction::Add {
                suite_dir,
                id,
                desc,
                from_savestate,
                advance,
                from_input,
                start,
                anchor,
                predicate,
                rom,
                expect,
            } => commands::regression::add(
                &suite_dir,
                &id,
                &desc,
                from_savestate.as_deref(),
                advance,
                from_input.as_deref(),
                start.as_deref(),
                anchor.as_deref(),
                &predicate,
                &rom,
                &expect,
            ),
        },
        Command::Track { action } => match action {
            TrackAction::Reindex => commands::track::reindex(),
            TrackAction::Import { bundle } => commands::track::import(&bundle),
            TrackAction::Ls { rom, goal } => commands::track::ls(rom.as_deref(), goal.as_deref()),
            TrackAction::Show { rom, run_id } => commands::track::show(&rom, &run_id),
            TrackAction::Compare { run_id_a, run_id_b } => {
                commands::track::compare(&run_id_a, &run_id_b)
            }
            TrackAction::Summarize { goal, tag, rom } => {
                commands::track::summarize(goal.as_deref(), tag.as_deref(), rom.as_deref())
            }
        },
    }
}
