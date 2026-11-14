//! Command line interface.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::gain;
use crate::lyrics::Lrclib;
use crate::pipeline::{Options, Outcome, Pipeline, Steps};
use crate::tools;
use crate::ui::Ui;

#[derive(Parser)]
#[command(name = "korg", version, about = "Kirby's music organizer: covers, synced lyrics, FLAC -> Opus, ReplayGain")]
pub struct Cli {
    /// Print every file that is checked, not only changes
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run all steps: covers, lyrics, encode, ReplayGain
    Run(RunArgs),
    /// Step 1 only: clean and resize cover.jpg files in the source library
    Covers(CommonArgs),
    /// Step 2 only: download synced .lrc files next to the source tracks
    Lyrics(CommonArgs),
    /// Step 3 only: FLAC -> Opus, copy MP3, then copy covers, .lrc and booklets
    Encode(CommonArgs),
    /// Step 4 only: ReplayGain for both libraries with rsgain
    Gain(CommonArgs),
    /// Undo path shortening left behind by an interrupted ReplayGain run
    RestoreNames,
    /// Check that opusenc and rsgain are installed and configured
    Doctor(ToolArgs),
}

#[derive(Args, Clone)]
pub struct ToolArgs {
    /// Path to opusenc (default: search PATH)
    #[arg(long, env = "KORG_OPUSENC", value_name = "PATH")]
    pub opusenc: Option<PathBuf>,

    /// Path to rsgain (default: search PATH)
    #[arg(long, env = "KORG_RSGAIN", value_name = "PATH")]
    pub rsgain: Option<PathBuf>,
}

#[derive(Args, Clone)]
pub struct CommonArgs {
    /// FLAC/MP3 library tagged with MusicBrainz Picard
    #[arg(short, long, env = "KORG_SOURCE", default_value = "toencode", value_name = "DIR")]
    pub source: PathBuf,

    /// Output library for Opus files
    #[arg(short, long, env = "KORG_DEST", default_value = "encoded", value_name = "DIR")]
    pub dest: PathBuf,

    /// Parallel encodes per album [default: CPU cores - 2]
    #[arg(short, long, value_name = "N")]
    pub jobs: Option<usize>,

    /// Keep an Opus file whose duration differs from the FLAC by more than 0.1 s (warn instead of fail)
    #[arg(long)]
    pub allow_duration_mismatch: bool,

    /// Show what would happen without changing anything
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    #[command(flatten)]
    pub tools: ToolArgs,
}

#[derive(Args, Clone)]
pub struct RunArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Steps to leave out (comma separated). Covers, lyrics and booklets are still copied when `encode` is skipped
    #[arg(long, value_enum, value_delimiter = ',', value_name = "STEP")]
    pub skip: Vec<Step>,
}

#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Covers,
    Lyrics,
    Encode,
    Gain,
}

fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(2).max(1))
}

fn options(common: CommonArgs, steps: Steps) -> Options {
    Options {
        source: common.source,
        dest: common.dest,
        steps,
        jobs: common.jobs.unwrap_or_else(default_jobs),
        allow_duration_mismatch: common.allow_duration_mismatch,
        dry_run: common.dry_run,
        opusenc: common.tools.opusenc,
        rsgain: common.tools.rsgain,
    }
}

fn steps_for(command: &Command) -> Option<Steps> {
    let only = |f: fn(&mut Steps)| {
        let mut steps = Steps::none();
        f(&mut steps);
        Some(steps)
    };
    match command {
        Command::Run(args) => {
            let mut steps = Steps::all();
            for step in &args.skip {
                match step {
                    Step::Covers => steps.covers = false,
                    Step::Lyrics => steps.lyrics = false,
                    Step::Encode => steps.encode = false,
                    Step::Gain => steps.gain = false,
                }
            }
            Some(steps)
        }
        Command::Covers(_) => only(|s| s.covers = true),
        Command::Lyrics(_) => only(|s| s.lyrics = true),
        Command::Encode(_) => only(|s| {
            s.encode = true;
            s.assets = true;
        }),
        Command::Gain(_) => only(|s| s.gain = true),
        Command::RestoreNames | Command::Doctor(_) => None,
    }
}

pub fn execute(cli: Cli) -> Result<ExitCode> {
    let ui = Ui::new(cli.verbose);
    match cli.command {
        Command::RestoreNames => {
            let journal = gain::journal_path();
            if journal.exists() {
                gain::restore_names(&journal)?;
                ui.ok("Original names restored.");
            } else {
                ui.info("No pending renames.");
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor(tools) => Ok(if tools::doctor(&ui, tools.opusenc.as_deref(), tools.rsgain.as_deref()) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }),
        command => {
            let steps = steps_for(&command).expect("pipeline command");
            let common = match command {
                Command::Run(args) => args.common,
                Command::Covers(c) | Command::Lyrics(c) | Command::Encode(c) | Command::Gain(c) => c,
                Command::RestoreNames | Command::Doctor(_) => unreachable!(),
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&cancel);
            // Only raise a flag: the pipeline restores renamed folders before exiting.
            ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))?;
            let api = Lrclib::new();
            let pipeline = Pipeline::new(&ui, &cancel, &api, gain::journal_path());
            let outcome = pipeline.run(&options(common, steps))?;
            Ok(match outcome {
                Outcome::Clean => ExitCode::SUCCESS,
                Outcome::Errors => ExitCode::FAILURE,
                Outcome::Cancelled => ExitCode::from(130),
            })
        }
    }
}
