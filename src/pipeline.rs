//! The four pipeline steps and the glue between them.
//!
//! 1. covers: clean `cover.jpg` files in the source library
//! 2. lyrics: fetch synced `.lrc` sidecars next to the source tracks
//! 3. encode: FLAC to Opus in the destination (MP3 is copied), then copy covers,
//!    sidecars and booklets
//! 4. gain: ReplayGain with `rsgain` for both libraries

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::audio;
use crate::cover;
use crate::encode::Encoder;
use crate::fsutil::{atomic_write, overlaps, resolve_path};
use crate::gain;
use crate::lyrics::{self, LyricsApi, Status};
use crate::tools;
use crate::ui::Ui;

pub const REPORT_NAME: &str = "korg-report.json";
const LYRICS_PAUSE: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Steps {
    pub covers: bool,
    pub lyrics: bool,
    pub encode: bool,
    /// Copy covers, sidecars and booklets into the destination.
    pub assets: bool,
    pub gain: bool,
}

impl Steps {
    pub fn all() -> Self {
        Self { covers: true, lyrics: true, encode: true, assets: true, gain: true }
    }

    pub fn none() -> Self {
        Self { covers: false, lyrics: false, encode: false, assets: false, gain: false }
    }

    fn uses_dest(&self) -> bool {
        self.encode || self.assets || self.gain
    }

    fn count(&self) -> usize {
        [self.covers, self.lyrics, self.encode, self.assets, self.gain].into_iter().filter(|&s| s).count()
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub source: PathBuf,
    pub dest: PathBuf,
    pub steps: Steps,
    pub jobs: usize,
    pub allow_duration_mismatch: bool,
    pub dry_run: bool,
    pub opusenc: Option<PathBuf>,
    pub rsgain: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Clean,
    Errors,
    Cancelled,
}

#[derive(Default)]
struct Counts {
    encoded: usize,
    copied_mp3: usize,
    skipped_albums: usize,
}

pub struct Pipeline<'a> {
    pub ui: &'a Ui,
    pub cancel: &'a AtomicBool,
    pub api: &'a dyn LyricsApi,
    pub journal: PathBuf,
    errors: Mutex<Vec<String>>,
}

impl<'a> Pipeline<'a> {
    pub fn new(ui: &'a Ui, cancel: &'a AtomicBool, api: &'a dyn LyricsApi, journal: PathBuf) -> Self {
        Self { ui, cancel, api, journal, errors: Mutex::new(Vec::new()) }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn fail(&self, stage: &str, path: &Path, err: impl std::fmt::Display) {
        let message = format!("{stage}: {}: {err}", path.display());
        self.ui.error(&message);
        self.errors.lock().expect("errors lock").push(message);
    }

    pub fn run(&self, opts: &Options) -> Result<Outcome> {
        let steps = opts.steps;
        if self.journal.exists() {
            bail!("Pending filename restoration ({}): run `korg restore-names` first", self.journal.display());
        }
        let source = resolve_path(&opts.source)?;
        let dest = resolve_path(&opts.dest)?;
        if !source.is_dir() {
            bail!("Source folder does not exist: {}", source.display());
        }
        if steps.uses_dest() && overlaps(&source, &dest) {
            bail!("Source and destination must not overlap: {} / {}", source.display(), dest.display());
        }
        if opts.jobs < 1 {
            bail!("--jobs must be positive");
        }
        let encoder = if steps.encode && !opts.dry_run {
            let path = tools::locate("opusenc", opts.opusenc.as_deref())?;
            Some(Encoder {
                opusenc: path,
                short_root: std::env::temp_dir(),
                allow_duration_mismatch: opts.allow_duration_mismatch,
            })
        } else {
            None
        };
        let rsgain =
            if steps.gain && !opts.dry_run { Some(tools::locate("rsgain", opts.rsgain.as_deref())?) } else { None };

        let files = list_files(&source)?;
        let tracks: Vec<PathBuf> = files.iter().filter(|p| is_track(p)).cloned().collect();
        if tracks.is_empty() {
            self.ui.info("No FLAC or MP3 files found.");
            return Ok(Outcome::Clean);
        }
        if opts.dry_run {
            self.ui.plain("Dry run: nothing will be changed.");
        }
        if steps.uses_dest() && !opts.dry_run {
            fs::create_dir_all(&dest).with_context(|| format!("create {}", dest.display()))?;
        }

        let total = steps.count();
        let mut number = 0;
        let mut next = |title: &str| {
            number += 1;
            self.ui.step(number, total, title);
        };
        let mut counts = Counts::default();
        let mut affected = BTreeSet::new();
        if steps.covers {
            next("Covers");
            self.covers(&source, &files, opts.dry_run);
        }
        if steps.lyrics && !self.cancelled() {
            next("Synced lyrics (LRCLIB)");
            self.lyrics(&source, &tracks, opts.dry_run);
        }
        if steps.encode && !self.cancelled() {
            next("Audio: FLAC -> Opus 160k VBR, MP3 -> copy");
            affected = self.encode(&source, &dest, &tracks, opts, encoder.as_ref(), &mut counts);
        }
        if steps.assets && !self.cancelled() {
            next("Covers, lyrics and booklets");
            self.assets(&source, &dest, opts.dry_run);
        }
        if steps.gain && !self.cancelled() {
            next("ReplayGain");
            self.gain(&source, &dest, &affected, rsgain.as_deref(), opts.dry_run);
        }
        // Runs on success, failure and Ctrl+C: never leave albums under their short names.
        if self.journal.exists()
            && let Err(err) = gain::restore_names(&self.journal)
        {
            self.fail("Restore names", &self.journal, format!("{err:#}"));
        }
        self.finish(opts, &dest, &counts)
    }

    fn finish(&self, opts: &Options, dest: &Path, counts: &Counts) -> Result<Outcome> {
        let errors = self.errors.lock().expect("errors lock").clone();
        if opts.steps.uses_dest() && !opts.dry_run && dest.is_dir() {
            let report = dest.join(REPORT_NAME);
            if errors.is_empty() {
                let _ = fs::remove_file(&report);
            } else {
                atomic_write(&report, &serde_json::to_vec_pretty(&serde_json::json!({ "errors": errors }))?)?;
            }
        }
        self.ui.plain("");
        let mut summary =
            format!("Done: encoded={}, copied_mp3={}, errors={}", counts.encoded, counts.copied_mp3, errors.len());
        if counts.skipped_albums > 0 {
            summary.push_str(&format!(", albums skipped={}", counts.skipped_albums));
        }
        if self.cancelled() {
            self.ui.warn(format!("Interrupted. {summary}"));
            return Ok(Outcome::Cancelled);
        }
        if errors.is_empty() {
            self.ui.ok(summary);
            return Ok(Outcome::Clean);
        }
        self.ui.error(summary);
        if opts.steps.uses_dest() && !opts.dry_run {
            self.ui.plain(format!("  Report: {}", opts.dest.join(REPORT_NAME).display()));
        }
        Ok(Outcome::Errors)
    }

    fn covers(&self, source: &Path, files: &[PathBuf], dry_run: bool) {
        let mut folders: BTreeSet<&Path> = files.iter().filter_map(|f| f.parent()).collect();
        folders.insert(source);
        let bar = self.ui.bar(folders.len() as u64, "covers");
        let (mut updated, mut unchanged) = (0, 0);
        for folder in folders {
            if self.cancelled() {
                break;
            }
            bar.inc(1);
            let found = match cover::candidates(folder) {
                Ok(found) => found,
                Err(err) => {
                    self.fail("Cover", folder, err);
                    continue;
                }
            };
            if found.len() > 1 {
                self.fail("Cover", folder, format!("Multiple cover candidates; choose one: {}", folder.display()));
                continue;
            }
            let Some(path) = found.first() else { continue };
            bar.set_message(rel(source, folder));
            match cover::process_cover(path, dry_run) {
                Ok(outcome) if outcome.changed => {
                    updated += 1;
                    let verb = if dry_run { "would update" } else { "updated" };
                    self.ui.ok(format!(
                        "{verb} {} ({} -> {} bytes)",
                        rel(source, &outcome.target),
                        outcome.before,
                        outcome.after
                    ));
                }
                Ok(outcome) => {
                    unchanged += 1;
                    self.ui.detail(format!("clean {}", rel(source, &outcome.target)));
                }
                Err(err) => self.fail("Cover", path, format!("{err:#}")),
            }
        }
        bar.finish_and_clear();
        self.ui.info(format!("covers: {updated} updated, {unchanged} already clean"));
    }

    fn lyrics(&self, source: &Path, tracks: &[PathBuf], dry_run: bool) {
        let bar = self.ui.bar(tracks.len() as u64, "lyrics");
        let (mut existing, mut downloaded, mut missing) = (0, 0, 0);
        for track in tracks {
            if self.cancelled() {
                break;
            }
            bar.inc(1);
            let key = rel(source, track);
            bar.set_message(key.clone());
            if dry_run {
                match lrc_state(track) {
                    LrcState::Valid => existing += 1,
                    LrcState::Missing | LrcState::Untimed => {
                        missing += 1;
                        self.ui.info(format!("would look up {key}"));
                    }
                }
                continue;
            }
            match lyrics::fetch_lyrics(track, &audio::read_info, self.api) {
                Ok(result) => {
                    if result.removed_untimed {
                        self.ui.warn(format!("removed LRC without timestamps: {key}"));
                    }
                    match result.status {
                        Status::Existing => {
                            existing += 1;
                            self.ui.detail(format!("existing {key}"));
                            continue;
                        }
                        Status::Downloaded => {
                            downloaded += 1;
                            self.ui.ok(format!("downloaded {key}"));
                        }
                        Status::NotFound => {
                            missing += 1;
                            self.ui.info(format!("no synced lyrics: {key}"));
                        }
                    }
                }
                Err(err) => self.fail("Lyrics", track, format!("{err:#}")),
            }
            std::thread::sleep(LYRICS_PAUSE);
        }
        bar.finish_and_clear();
        let missing_label = if dry_run { "to look up" } else { "not found" };
        self.ui.info(format!("lyrics: {existing} existing, {downloaded} downloaded, {missing} {missing_label}"));
    }

    fn encode(
        &self,
        source: &Path,
        dest: &Path,
        tracks: &[PathBuf],
        opts: &Options,
        encoder: Option<&Encoder>,
        counts: &mut Counts,
    ) -> BTreeSet<PathBuf> {
        let mut affected = BTreeSet::new();
        let mut albums: BTreeMap<&Path, Vec<&PathBuf>> = BTreeMap::new();
        for track in tracks {
            if let Some(parent) = track.parent() {
                albums.entry(parent).or_default().push(track);
            }
        }
        let mut todo = Vec::new();
        for (album, members) in albums {
            let relative = album.strip_prefix(source).unwrap_or(album).to_path_buf();
            if destination_exists(source, dest, album) {
                counts.skipped_albums += 1;
                self.ui.detail(format!("skip {} (destination folder exists)", rel_or_root(&relative)));
            } else {
                todo.push((relative, members));
            }
        }
        if counts.skipped_albums > 0 {
            self.ui.info(format!("{} albums skipped: destination folder exists", counts.skipped_albums));
        }
        let total: usize = todo.iter().map(|(_, members)| members.len()).sum();
        if opts.dry_run {
            for (relative, members) in &todo {
                self.ui.info(format!("would encode {} ({} tracks)", rel_or_root(relative), members.len()));
            }
            return affected;
        }
        let Some(encoder) = encoder else { return affected };
        let pool = match rayon::ThreadPoolBuilder::new().num_threads(opts.jobs).build() {
            Ok(pool) => pool,
            Err(err) => {
                self.fail("Encode", source, err);
                return affected;
            }
        };
        let bar = self.ui.bar(total as u64, "encode");
        for (relative, members) in todo {
            if self.cancelled() {
                break;
            }
            bar.set_message(rel_or_root(&relative));
            let flacs: Vec<&PathBuf> = members.iter().copied().filter(|t| !is_mp3(t)).collect();
            for track in members.iter().copied().filter(|t| is_mp3(t)) {
                let target = dest.join(track.strip_prefix(source).unwrap_or(track));
                match fs::read(track).map_err(anyhow::Error::from).and_then(|data| atomic_write(&target, &data)) {
                    Ok(()) => counts.copied_mp3 += 1,
                    Err(err) => self.fail("Copy MP3", track, format!("{err:#}")),
                }
                bar.inc(1);
            }
            let results: Vec<Option<Result<crate::encode::Encoded>>> = pool.install(|| {
                flacs
                    .par_iter()
                    .map(|track| {
                        if self.cancelled() {
                            return None;
                        }
                        let target = dest.join(track.strip_prefix(source).unwrap_or(track)).with_extension("opus");
                        let result = encoder.encode(track, &target);
                        bar.inc(1);
                        Some(result)
                    })
                    .collect()
            });
            for (track, result) in flacs.iter().zip(results) {
                match result {
                    Some(Ok(done)) => {
                        counts.encoded += 1;
                        if done.used_short_path {
                            self.ui.detail(format!("encoded via short path: {}", rel(source, track)));
                            affected.insert(relative.clone());
                        }
                        if let Some(warning) = done.warning {
                            self.ui.warn(warning);
                        }
                    }
                    Some(Err(err)) => self.fail("Encode", track, format!("{err:#}")),
                    None => {}
                }
            }
            self.ui.ok(format!("{} ({} tracks)", rel_or_root(&relative), members.len()));
        }
        bar.finish_and_clear();
        self.ui.info(format!("encoded {}, copied {} MP3", counts.encoded, counts.copied_mp3));
        affected
    }

    /// Copies covers, sidecar lyrics and booklets next to the destination tracks.
    fn assets(&self, source: &Path, dest: &Path, dry_run: bool) {
        // Listed again on purpose: the covers step renames files and the lyrics step adds
        // new sidecars, and both must be picked up here.
        let files = match list_files(source) {
            Ok(files) => files,
            Err(err) => {
                self.fail("Copy", source, format!("{err:#}"));
                return;
            }
        };
        let (mut copied, mut unchanged) = (0, 0);
        for asset in files.iter().filter(|f| is_asset(f)) {
            if self.cancelled() {
                break;
            }
            let target = dest.join(asset.strip_prefix(source).unwrap_or(asset));
            let outcome = (|| -> Result<bool> {
                let data = fs::read(asset)?;
                if fs::read(&target).is_ok_and(|existing| existing == data) {
                    return Ok(false);
                }
                if !dry_run {
                    atomic_write(&target, &data)?;
                }
                Ok(true)
            })();
            match outcome {
                Ok(true) => {
                    copied += 1;
                    self.ui.detail(format!("copy {}", rel(source, asset)));
                }
                Ok(false) => unchanged += 1,
                Err(err) => self.fail("Copy", asset, format!("{err:#}")),
            }
        }
        let verb = if dry_run { "to copy" } else { "copied" };
        self.ui.info(format!("assets: {copied} {verb}, {unchanged} up to date"));
    }

    fn gain(&self, source: &Path, dest: &Path, affected: &BTreeSet<PathBuf>, rsgain: Option<&Path>, dry_run: bool) {
        let commands = gain::rsgain_commands(source, dest);
        if dry_run {
            for args in &commands {
                self.ui.info(format!("would run: rsgain {}", args.join(" ")));
            }
            return;
        }
        let Some(rsgain) = rsgain else { return };
        if !affected.is_empty() {
            self.ui.info("Temporarily shortening affected album paths");
            if let Err(err) = gain::shorten_for_replaygain(source, dest, affected, &self.journal) {
                self.fail("Shorten paths", source, format!("{err:#}"));
                return;
            }
        }
        // Both scans always run, even when the first fails.
        for (args, root) in commands.iter().zip([source, dest]) {
            self.ui.info(format!("rsgain {}", args.join(" ")));
            match self.ui.suspend(|| gain::run_rsgain(rsgain, args)) {
                Ok(()) => self.ui.ok(format!("ReplayGain done: {}", root.display())),
                Err(err) => self.fail("ReplayGain", root, format!("{err:#}")),
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LrcState {
    Missing,
    Untimed,
    Valid,
}

fn lrc_state(track: &Path) -> LrcState {
    match fs::read(track.with_extension("lrc")) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)).into_owned();
            if lyrics::has_timestamps(&text) { LrcState::Valid } else { LrcState::Untimed }
        }
        Err(_) => LrcState::Missing,
    }
}

/// An album is skipped when its destination folder already exists. Tracks that sit directly in
/// the source root share the destination root, which always exists, so for them the
/// presence of encoded audio there is the signal instead.
fn destination_exists(source: &Path, dest: &Path, album: &Path) -> bool {
    let target = dest.join(album.strip_prefix(source).unwrap_or(album));
    if album != source {
        return target.is_dir();
    }
    fs::read_dir(&target).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            name.ends_with(".opus") || name.ends_with(".mp3")
        })
    })
}

pub fn list_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).sort_by_file_name() {
        let entry = entry?;
        if entry.file_type().is_file() {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}

fn extension(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn is_track(path: &Path) -> bool {
    matches!(extension(path).as_str(), "flac" | "mp3")
}

fn is_mp3(path: &Path) -> bool {
    extension(path) == "mp3"
}

fn is_asset(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    name == "cover.jpg" || name == "booklet.pdf" || extension(path) == "lrc"
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}

fn rel_or_root(relative: &Path) -> String {
    if relative.as_os_str().is_empty() { ".".to_string() } else { relative.display().to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_files() {
        assert!(is_track(Path::new("a/B.FLAC")));
        assert!(is_track(Path::new("a/b.mp3")));
        assert!(!is_track(Path::new("a/b.opus")));
        assert!(is_asset(Path::new("a/Cover.JPG")));
        assert!(is_asset(Path::new("a/Booklet.pdf")));
        assert!(is_asset(Path::new("a/song.LRC")));
        assert!(!is_asset(Path::new("a/front.jpg")));
    }

    #[test]
    fn root_level_albums_are_skipped_only_when_encoded_audio_exists() {
        let dir = tempfile::tempdir().unwrap();
        let (source, dest) = (dir.path().join("src"), dir.path().join("dst"));
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();
        assert!(!destination_exists(&source, &dest, &source));
        fs::write(dest.join("a.opus"), "x").unwrap();
        assert!(destination_exists(&source, &dest, &source));
        assert!(!destination_exists(&source, &dest, &source.join("album")));
        fs::create_dir_all(dest.join("album")).unwrap();
        assert!(destination_exists(&source, &dest, &source.join("album")));
    }
}
