//! Terminal output: step headers, status lines and progress bars.
//!
//! Everything goes to stderr. Colors follow `NO_COLOR` and TTY detection (via `console`),
//! and without a TTY progress bars are hidden while status lines still print.

use std::time::Duration;

use console::{Emoji, Term, style};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

pub struct Ui {
    multi: MultiProgress,
    tty: bool,
    verbose: bool,
}

impl Ui {
    pub fn new(verbose: bool) -> Self {
        let tty = Term::stderr().is_term();
        let multi = MultiProgress::with_draw_target(ProgressDrawTarget::stderr_with_hz(15));
        Self { multi, tty, verbose }
    }

    pub fn verbose(&self) -> bool {
        self.verbose
    }

    fn emit(&self, line: String) {
        if self.tty {
            let _ = self.multi.println(line);
        } else {
            eprintln!("{line}");
        }
    }

    pub fn step(&self, number: usize, total: usize, title: &str) {
        self.emit(format!("\n{} {}", style(format!("[{number}/{total}]")).cyan().bold(), style(title).bold()));
    }

    pub fn ok(&self, message: impl AsRef<str>) {
        self.emit(format!("  {} {}", style(Emoji("✔", "+")).green(), message.as_ref()));
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        self.emit(format!("  {} {}", style(Emoji("▲", "!")).yellow(), message.as_ref()));
    }

    pub fn error(&self, message: impl AsRef<str>) {
        self.emit(format!("  {} {}", style(Emoji("✘", "x")).red().bold(), message.as_ref()));
    }

    /// Neutral status line.
    pub fn info(&self, message: impl AsRef<str>) {
        self.emit(format!("  {} {}", style(Emoji("•", "-")).dim(), style(message.as_ref()).dim()));
    }

    /// Shown only with `--verbose`.
    pub fn detail(&self, message: impl AsRef<str>) {
        if self.verbose {
            self.info(message);
        }
    }

    pub fn plain(&self, message: impl AsRef<str>) {
        self.emit(message.as_ref().to_string());
    }

    pub fn bar(&self, len: u64, label: &str) -> ProgressBar {
        let bar = self.multi.add(ProgressBar::new(len));
        bar.set_style(
            ProgressStyle::with_template("  {spinner:.cyan} {prefix:<9} [{bar:30.cyan/blue}] {pos}/{len}  {wide_msg}")
                .expect("valid template")
                .progress_chars("=> "),
        );
        bar.set_prefix(label.to_string());
        bar.enable_steady_tick(Duration::from_millis(120));
        bar
    }

    /// Runs `f` with the progress bars paused, for child processes that print to the terminal.
    pub fn suspend<R>(&self, f: impl FnOnce() -> R) -> R {
        self.multi.suspend(f)
    }
}
