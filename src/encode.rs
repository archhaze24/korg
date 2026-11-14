//! Step 3: FLAC to Opus through `opusenc`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::audio;
use crate::fsutil::copy_file;

/// Maximum accepted difference between source and output duration, in seconds.
pub const MAX_DURATION_DRIFT: f64 = 0.1;

/// Messages external tools print when they cannot open an input path (English and the
/// Russian Windows wording the library was first built on).
const NOT_FOUND: [&str; 6] = [
    "no such file or directory",
    "cannot find the file",
    "cannot find the path",
    "file not found",
    "path not found",
    "не удается найти указанный",
];

/// True only for a failed run whose error text says the input path was not found.
pub fn input_not_found(success: bool, stderr: &str) -> bool {
    if success {
        return false;
    }
    let message = stderr.to_lowercase();
    NOT_FOUND.iter().any(|text| message.contains(text))
}

#[derive(Debug)]
pub struct Encoded {
    /// The encode only worked after copying the input to a short temporary path.
    pub used_short_path: bool,
    /// Set when the duration drifted but `allow_duration_mismatch` kept the file.
    pub warning: Option<String>,
}

pub struct Encoder {
    pub opusenc: PathBuf,
    /// Where short-path retries stage their copies; should be a short location.
    pub short_root: PathBuf,
    pub allow_duration_mismatch: bool,
}

struct Run {
    success: bool,
    stderr: String,
}

impl Encoder {
    fn run(&self, input: &Path, output: &Path) -> Result<Run> {
        let result = Command::new(&self.opusenc)
            .args(["--quiet", "--bitrate", "160", "--vbr"])
            .arg(input)
            .arg(output)
            .output()
            .with_context(|| format!("start {}", self.opusenc.display()))?;
        Ok(Run { success: result.status.success(), stderr: String::from_utf8_lossy(&result.stderr).into_owned() })
    }

    /// Encodes `src` to `dst`. The final file appears atomically, and `dst` is untouched on failure.
    pub fn encode(&self, src: &Path, dst: &Path) -> Result<Encoded> {
        let parent = dst.parent().context("destination has no parent")?;
        fs::create_dir_all(parent)?;
        let work = tempfile::Builder::new().prefix(".korg-").tempdir_in(parent)?;
        let original_length = audio::duration(src)?;

        // Tags of the source stay as they are, even after earlier ReplayGain runs: the
        // encoder reads a gain-free temporary copy instead.
        let mut input = src.to_path_buf();
        if audio::flac_has_gain_tags(src)? {
            input = work.path().join("input.flac");
            copy_file(src, &input)?;
            audio::strip_flac_gain_tags(&input)?;
        }
        let mut output = work.path().join("output.opus");
        let mut run = self.run(&input, &output)?;
        let mut short_dir = None;
        let mut used_short_path = false;
        // Retry only after a real "not found" error and only when the input exists.
        if input_not_found(run.success, &run.stderr) && input.is_file() {
            let dir = tempfile::Builder::new().prefix(".enc-").tempdir_in(&self.short_root)?;
            let short_input = dir.path().join("in.flac");
            copy_file(&input, &short_input)?;
            output = dir.path().join("out.opus");
            run = self.run(&short_input, &output)?;
            used_short_path = true;
            short_dir = Some(dir);
        }
        if !run.success {
            bail!("{}: {}", src.display(), run.stderr.trim());
        }
        audio::clean_opus_tags(&output)?;
        let encoded_length = audio::duration(&output)?;
        let mut warning = None;
        if (encoded_length - original_length).abs() > MAX_DURATION_DRIFT {
            let message = format!(
                "Output duration mismatch: {}; FLAC={original_length:.6}s, Opus={encoded_length:.6}s, difference={:+.6}s",
                src.display(),
                encoded_length - original_length
            );
            if !self.allow_duration_mismatch {
                bail!(message);
            }
            warning = Some(message);
        }
        // Move onto the destination volume first, then replace atomically (the short
        // directory may live on another volume).
        if used_short_path {
            let staged = work.path().join("ready.opus");
            copy_file(&output, &staged)?;
            output = staged;
        }
        fs::rename(&output, dst).with_context(|| format!("replace {}", dst.display()))?;
        drop(short_dir);
        Ok(Encoded { used_short_path, warning })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_path_retry_only_for_not_found() {
        assert!(input_not_found(false, "Cannot open input file: No such file or directory"));
        assert!(input_not_found(false, "Не удается найти указанный файл."));
        assert!(input_not_found(false, "Не удается найти указанный путь."));
        assert!(!input_not_found(true, "No such file or directory"));
        for message in ["Permission denied", "Invalid FLAC stream", "Output duration mismatch"] {
            assert!(!input_not_found(false, message), "{message}");
        }
    }
}
