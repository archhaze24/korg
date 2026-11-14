//! Locating and checking the external tools: `opusenc` and `rsgain`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, anyhow};
use console::strip_ansi_codes;

use crate::gain;
use crate::ui::Ui;

fn install_hint(name: &str) -> &'static str {
    match name {
        "opusenc" => {
            "Install opus-tools (https://opus-codec.org/downloads/): `brew install opus-tools`, `apt install opus-tools`, or the Windows zip."
        }
        "rsgain" => {
            "Install rsgain (https://github.com/complexlogic/rsgain): `brew install rsgain`, your distro package, or the Windows release."
        }
        _ => "",
    }
}

/// Resolves a tool from an explicit path (flag or environment) or from `PATH`.
pub fn locate(name: &str, explicit: Option<&Path>) -> Result<PathBuf> {
    let lookup = explicit.map_or_else(|| name.to_string(), |p| p.display().to_string());
    which::which(&lookup).map_err(|_| match explicit {
        Some(path) => anyhow!("{name} not found at {}", path.display()),
        None => anyhow!("`{name}` is not installed or not in PATH. {}", install_hint(name)),
    })
}

fn first_line(command: &mut Command) -> String {
    match command.output() {
        Ok(out) => {
            let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            strip_ansi_codes(text.lines().next().unwrap_or("")).trim().to_string()
        }
        Err(err) => format!("failed to run: {err}"),
    }
}

/// True when `rsgain` accepts `-p opus`. An empty scan folder is enough to find out.
fn opus_preset_installed(rsgain: &Path) -> Result<bool> {
    let dir = tempfile::tempdir()?;
    let out = Command::new(rsgain).args(["easy", "-q", "-m", "MAX", "-p", "opus"]).arg(dir.path()).output()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    Ok(out.status.success() && !text.contains("Could not locate preset"))
}

/// Prints what is installed and what is missing. Returns true when everything needed works.
pub fn doctor(ui: &Ui, opusenc: Option<&Path>, rsgain: Option<&Path>) -> bool {
    let mut healthy = true;
    match locate("opusenc", opusenc) {
        Ok(path) => {
            ui.ok(format!("opusenc: {} ({})", path.display(), first_line(Command::new(&path).arg("--version"))))
        }
        Err(err) => {
            healthy = false;
            ui.error(format!("{err}"));
        }
    }
    match locate("rsgain", rsgain) {
        Ok(path) => {
            ui.ok(format!("rsgain: {} ({})", path.display(), first_line(Command::new(&path).arg("--version"))));
            match opus_preset_installed(&path) {
                Ok(true) => ui.ok("rsgain preset `opus` is installed"),
                Ok(false) => {
                    healthy = false;
                    ui.error(
                        "rsgain preset `opus` not found: create `opus.ini` in rsgain's presets folder (see README)",
                    );
                }
                Err(err) => ui.warn(format!("could not check the rsgain preset: {err}")),
            }
        }
        Err(err) => {
            healthy = false;
            ui.error(format!("{err}"));
        }
    }
    let journal = gain::journal_path();
    if journal.exists() {
        healthy = false;
        ui.warn(format!("pending rename journal {}: run `korg restore-names`", journal.display()));
    } else {
        ui.ok("no pending renames");
    }
    healthy
}
