//! End-to-end runs against real `opusenc`, `rsgain`, `flac` and `lame`.
//!
//! Skipped (with a note) when one of the tools is missing. They never touch the network:
//! every track already has a timestamped `.lrc` sidecar.
//!
//! rsgain has no bundled `opus` preset, so the tests install the one from the README into a
//! private `HOME`.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use lofty::config::ParseOptions;
use lofty::file::AudioFile;
use lofty::flac::FlacFile;
use lofty::ogg::{OggPictureStorage, OpusFile};

const LRC: &str = "[00:01.00] la la\n";

fn have(tool: &str) -> bool {
    which::which(tool).is_ok()
}

fn tools_available() -> bool {
    let missing: Vec<_> = ["opusenc", "rsgain", "flac", "lame"].into_iter().filter(|t| !have(t)).collect();
    if !missing.is_empty() {
        // CI sets this so a broken tool install fails the job instead of skipping silently.
        assert!(std::env::var_os("KORG_E2E_REQUIRE").is_none(), "e2e tools missing: {missing:?}");
        eprintln!("skipping e2e: missing {missing:?}");
    }
    missing.is_empty()
}

fn wav(path: &Path, seconds: f64, hz: f64) {
    let rate = 44_100u32;
    let frames = (f64::from(rate) * seconds) as u32;
    let mut data = Vec::new();
    for i in 0..frames {
        let t = f64::from(i) / f64::from(rate);
        let left = (12_000.0 * (2.0 * std::f64::consts::PI * hz * t).sin()) as i16;
        let right = (9_000.0 * (2.0 * std::f64::consts::PI * hz * 1.5 * t).sin()) as i16;
        data.extend_from_slice(&left.to_le_bytes());
        data.extend_from_slice(&right.to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    fs::write(path, out).unwrap();
}

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let img = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8])
    });
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95).encode_image(&img).unwrap();
    out
}

fn run_ok(command: &mut Command) {
    let out = command.output().unwrap();
    assert!(out.status.success(), "{command:?}\n{}", String::from_utf8_lossy(&out.stderr));
}

/// Tagged FLAC with an embedded picture and a stale ReplayGain tag.
fn flac(scratch: &Path, path: &Path, title: &str, seconds: f64, hz: f64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let input = scratch.join("in.wav");
    wav(&input, seconds, hz);
    let picture = scratch.join("pic.jpg");
    fs::write(&picture, jpeg(64, 64)).unwrap();
    run_ok(
        Command::new("flac")
            .args(["-s", "-f"])
            .arg(format!("--tag=TITLE={title}"))
            .args([
                "--tag=ARTIST=Test Artist",
                "--tag=ALBUM=Test Album",
                "--tag=REPLAYGAIN_TRACK_GAIN=-9.99 dB",
                "--tag=Custom_Field=keep me",
            ])
            .arg(format!("--picture={}", picture.display()))
            .arg("-o")
            .arg(path)
            .arg(&input),
    );
    fs::write(path.with_extension("lrc"), LRC).unwrap();
}

fn mp3(scratch: &Path, path: &Path) {
    let input = scratch.join("in.wav");
    wav(&input, 2.0, 523.0);
    run_ok(
        Command::new("lame")
            .args(["--quiet", "--tt", "Mp3 Song", "--ta", "Test Artist", "--tl", "Test Album"])
            .arg(&input)
            .arg(path),
    );
    fs::write(path.with_extension("lrc"), LRC).unwrap();
}

struct Fixture {
    root: tempfile::TempDir,
    source: PathBuf,
    dest: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("toencode");
        let dest = root.path().join("encoded");
        let scratch = root.path().join("scratch");
        fs::create_dir_all(&scratch).unwrap();
        let album = source.join("Test Artist").join("2001 - Test Album");
        flac(&scratch, &album.join("01. First.flac"), "First", 3.0, 440.0);
        mp3(&scratch, &album.join("02. Second.mp3"));
        fs::write(album.join("Cover.JPG"), jpeg(2400, 1800)).unwrap();
        // Multi-disc release: cover and booklet live in the parent folder.
        let multi = source.join("Test Artist").join("2002 - Multi");
        flac(&scratch, &multi.join("CD1").join("01. One.flac"), "One", 2.5, 330.0);
        flac(&scratch, &multi.join("CD2").join("01. Two.flac"), "Two", 2.0, 220.0);
        fs::write(multi.join("cover.jpg"), jpeg(300, 300)).unwrap();
        fs::write(multi.join("booklet.pdf"), b"%PDF-1.4 fake").unwrap();
        fs::remove_dir_all(&scratch).unwrap();
        Self { root, source, dest }
    }

    fn korg(&self, args: &[&str]) -> Output {
        // rsgain looks for presets under $HOME/.config on Linux and macOS.
        let home = self.root.path().join("home");
        let presets = home.join(".config").join("rsgain").join("presets");
        fs::create_dir_all(&presets).unwrap();
        fs::write(presets.join("opus.ini"), "[Opus]\nOpusMode=r\n").unwrap();
        Command::new(env!("CARGO_BIN_EXE_korg"))
            .args(args)
            .arg("--source")
            .arg(&self.source)
            .arg("--dest")
            .arg(&self.dest)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    }

    fn korg_ok(&self, args: &[&str]) -> String {
        let out = self.korg(args);
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "korg {args:?} failed:\n{text}");
        text
    }

    /// Relative path -> file content, for every file below `root`.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        korg::pipeline::list_files(root)
            .unwrap()
            .into_iter()
            .map(|p| (p.strip_prefix(root).unwrap().to_path_buf(), fs::read(&p).unwrap()))
            .collect()
    }
}

fn opus_comments(path: &Path) -> (Vec<(String, String)>, usize) {
    let mut file = fs::File::open(path).unwrap();
    let opus = OpusFile::read_from(&mut file, ParseOptions::new()).unwrap();
    let tag = opus.vorbis_comments();
    (tag.items().map(|(k, v)| (k.to_string(), v.to_string())).collect(), tag.pictures().len())
}

fn flac_comments(path: &Path) -> Vec<(String, String)> {
    let mut file = fs::File::open(path).unwrap();
    let flac = FlacFile::read_from(&mut file, ParseOptions::new()).unwrap();
    flac.vorbis_comments().unwrap().items().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn has_key(items: &[(String, String)], key: &str) -> bool {
    items.iter().any(|(k, _)| k.eq_ignore_ascii_case(key))
}

#[test]
fn encode_strips_gain_tags_and_keeps_everything_else() {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new();
    fx.korg_ok(&["encode"]);

    let encoded = fx.dest.join("Test Artist/2001 - Test Album/01. First.opus");
    let (tags, pictures) = opus_comments(&encoded);
    assert!(!tags.iter().any(|(k, _)| korg::audio::is_gain_tag(k)), "gain tags left: {tags:?}");
    assert!(!has_key(&tags, "encoder") && !has_key(&tags, "encoder_options"), "{tags:?}");
    assert!(tags.contains(&("TITLE".into(), "First".into())), "{tags:?}");
    assert!(
        tags.contains(&("Custom_Field".into(), "keep me".into()))
            || tags.iter().any(|(k, v)| k.eq_ignore_ascii_case("custom_field") && v == "keep me"),
        "{tags:?}"
    );
    assert_eq!(pictures, 1, "embedded picture lost");

    // The source keeps its own (stale) gain tag: only the encoder input was cleaned.
    assert!(has_key(
        &flac_comments(&fx.source.join("Test Artist/2001 - Test Album/01. First.flac")),
        "replaygain_track_gain"
    ));

    // The tag rewrite must leave a stream the reference decoder still accepts.
    if have("opusdec") {
        run_ok(Command::new("opusdec").arg("--quiet").arg(&encoded).arg("/dev/null"));
    }
    let duration = korg::audio::duration(&encoded).unwrap();
    assert!((duration - 3.0).abs() < 0.1, "{duration}");

    // MP3 is copied byte for byte, assets are copied, and the cover lands under its canonical name.
    let mp3_src = fs::read(fx.source.join("Test Artist/2001 - Test Album/02. Second.mp3")).unwrap();
    assert_eq!(mp3_src, fs::read(fx.dest.join("Test Artist/2001 - Test Album/02. Second.mp3")).unwrap());
    assert!(fx.dest.join("Test Artist/2002 - Multi/booklet.pdf").is_file());
    assert!(fx.dest.join("Test Artist/2002 - Multi/cover.jpg").is_file());
    assert!(fx.dest.join("Test Artist/2002 - Multi/CD2/01. Two.opus").is_file());
    assert!(fx.dest.join("Test Artist/2002 - Multi/CD1/01. One.lrc").is_file());
    assert!(!fx.dest.join("korg-report.json").exists());
    // Without the covers step nothing renames the source cover, so it is copied under its own name.
    assert!(fx.dest.join("Test Artist/2001 - Test Album/Cover.JPG").is_file());
}

#[test]
fn full_run_writes_gain_and_second_run_changes_nothing() {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new();
    fx.korg_ok(&["run", "--skip", "lyrics"]);

    // Cover step: the odd-cased cover was renamed, resized and left no duplicate behind.
    let album = fx.source.join("Test Artist/2001 - Test Album");
    let names: Vec<_> =
        fs::read_dir(&album).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(
        names.contains(&"cover.jpg".to_string())
            && !names.iter().any(|n| n.eq_ignore_ascii_case("cover.jpg") && n != "cover.jpg"),
        "{names:?}"
    );
    assert_eq!(image::image_dimensions(album.join("cover.jpg")).unwrap(), (2000, 1500));
    assert!(fx.dest.join("Test Artist/2001 - Test Album/cover.jpg").is_file());

    // Gain step: R128 tags in the Opus library, ReplayGain tags in the FLAC source.
    let (tags, pictures) = opus_comments(&fx.dest.join("Test Artist/2002 - Multi/CD1/01. One.opus"));
    assert!(has_key(&tags, "R128_TRACK_GAIN") && has_key(&tags, "R128_ALBUM_GAIN"), "{tags:?}");
    assert_eq!(pictures, 1);
    // rsgain ignores the `[Opus]` preset section in a folder that mixes Opus and MP3 and
    // writes standard ReplayGain tags there; either kind proves the folder was scanned.
    let (mixed, _) = opus_comments(&fx.dest.join("Test Artist/2001 - Test Album/01. First.opus"));
    assert!(mixed.iter().any(|(k, _)| korg::audio::is_gain_tag(k)), "{mixed:?}");
    let source_tags = flac_comments(&album.join("01. First.flac"));
    assert!(has_key(&source_tags, "replaygain_album_gain"), "{source_tags:?}");

    let before = (Fixture::snapshot(&fx.source), Fixture::snapshot(&fx.dest));
    let second = fx.korg_ok(&["run"]);
    let after = (Fixture::snapshot(&fx.source), Fixture::snapshot(&fx.dest));
    for (name, (old, new)) in [("source", (&before.0, &after.0)), ("destination", (&before.1, &after.1))] {
        let changed: Vec<_> =
            old.iter().filter(|(path, data)| new.get(*path) != Some(*data)).map(|(path, _)| path).collect();
        assert!(
            changed.is_empty() && old.len() == new.len(),
            "{name} changed on the second run: {changed:?}\n{second}"
        );
    }
    assert!(second.contains("3 albums skipped"), "{second}");
}

#[test]
fn dry_run_changes_nothing() {
    if !tools_available() {
        return;
    }
    let fx = Fixture::new();
    let before = Fixture::snapshot(&fx.source);
    let text = fx.korg_ok(&["run", "--dry-run"]);
    assert_eq!(before, Fixture::snapshot(&fx.source));
    assert!(!fx.dest.exists());
    assert!(text.contains("would encode"), "{text}");
}

/// A wrapper that pretends the path is too long for the first attempt, to exercise the
/// short-path retry and the temporary renaming around ReplayGain.
#[cfg(unix)]
#[test]
fn short_path_retry_renames_for_gain_and_restores() {
    use std::os::unix::fs::PermissionsExt;
    if !tools_available() {
        return;
    }
    let fx = Fixture::new();
    let wrapper = fx.root.path().join("opusenc-wrapper.sh");
    let real = which::which("opusenc").unwrap();
    fs::write(
        &wrapper,
        format!("#!/bin/sh\ncase \"$*\" in *'2001 - Test Album'*) echo 'cannot open: No such file or directory' >&2; exit 1;; esac\nexec '{}' \"$@\"\n", real.display()),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let text = fx.korg_ok(&["run", "--skip", "lyrics,covers", "--opusenc", wrapper.to_str().unwrap(), "-v"]);
    assert!(text.contains("encoded via short path"), "{text}");
    assert!(text.contains("Temporarily shortening"), "{text}");
    assert!(fx.dest.join("Test Artist/2001 - Test Album/01. First.opus").is_file());
    assert!(fx.source.join("Test Artist/2001 - Test Album/01. First.flac").is_file());
    let leftovers =
        |root: &Path| fs::read_dir(root).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".rg-"));
    assert!(!leftovers(&fx.source) && !leftovers(&fx.dest));
    let (tags, _) = opus_comments(&fx.dest.join("Test Artist/2001 - Test Album/01. First.opus"));
    assert!(tags.iter().any(|(k, _)| korg::audio::is_gain_tag(k)), "{tags:?}");
}
