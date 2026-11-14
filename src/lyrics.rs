//! Step 2: synchronized lyrics from LRCLIB, stored as `.lrc` sidecars.
//!
//! Only lyrics with timestamps are kept. A record counts as a match when title and artist
//! agree (ignoring case, punctuation and Unicode variants) and its duration is within two
//! seconds of the file. The album is only a tie-breaker, so a single released on a
//! compilation still finds its lyrics.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};

use crate::audio::TrackInfo;
use crate::fsutil::atomic_write;

pub const MAX_DURATION_DIFF: f64 = 2.0;

fn timestamp() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\d+:\d{2}(?:[.:]\d{1,3})?\]").expect("valid regex"))
}

pub fn has_timestamps(text: &str) -> bool {
    timestamp().is_match(text)
}

/// NFKC + case folding, keeping only letters and digits.
pub fn normalize(value: &str) -> String {
    let folded = caseless::default_case_fold_str(&value.nfkc().collect::<String>());
    folded
        .chars()
        .filter(|c| matches!(c.general_category_group(), GeneralCategoryGroup::Letter | GeneralCategoryGroup::Number))
        .collect()
}

/// Access to the LRCLIB endpoints the pipeline uses; a trait so tests can fake it.
pub trait LyricsApi: Sync {
    /// `GET /api/get`; `None` on 404.
    fn get(&self, track: &TrackInfo) -> Result<Option<Value>>;
    /// `GET /api/search` by title and artist only; `None` on 404.
    fn search(&self, track: &TrackInfo) -> Result<Option<Value>>;
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Status {
    Existing,
    Downloaded,
    NotFound,
}

#[derive(Debug)]
pub struct LyricsResult {
    pub status: Status,
    /// An old sidecar without timestamps was deleted before the lookup.
    pub removed_untimed: bool,
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn field<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}

fn matches(item: &Value, track: &TrackInfo, require_album: bool) -> bool {
    if !item.is_object() {
        return false;
    }
    let Some(duration) = item.get("duration").and_then(number) else {
        return false;
    };
    if !duration.is_finite() || duration <= 0.0 {
        return false;
    }
    let synced = field(item, "syncedLyrics");
    normalize(field(item, "trackName")) == normalize(&track.title)
        && normalize(field(item, "artistName")) == normalize(&track.artist)
        && (!require_album || normalize(field(item, "albumName")) == normalize(&track.album))
        && (duration - track.length).abs() <= MAX_DURATION_DIFF
        && has_timestamps(synced)
}

/// Picks the best synced lyrics for `track`, or `None`.
fn find_record(track: &TrackInfo, api: &dyn LyricsApi) -> Result<Option<String>> {
    let exact = api.get(track)?;
    if let Some(record) = exact.filter(|r| matches(r, track, true)) {
        return Ok(Some(field(&record, "syncedLyrics").to_string()));
    }
    let records = api.search(track)?;
    let candidates: Vec<&Value> = match &records {
        Some(Value::Array(items)) => items.iter().filter(|item| matches(item, track, false)).collect(),
        _ => Vec::new(),
    };
    let album = normalize(&track.album);
    // Closest duration wins; the original album only breaks ties. `min_by` keeps the first
    // of equal candidates, which preserves LRCLIB's own ordering.
    let best = candidates.into_iter().min_by(|a, b| {
        let key = |item: &Value| {
            let diff = (item.get("duration").and_then(number).unwrap_or(0.0) - track.length).abs();
            (diff, normalize(field(item, "albumName")) != album)
        };
        let (da, aa) = key(a);
        let (db, ab) = key(b);
        da.total_cmp(&db).then(aa.cmp(&ab))
    });
    Ok(best.map(|item| field(item, "syncedLyrics").to_string()))
}

/// Looks up lyrics for one track and writes the `.lrc` sidecar next to it.
pub fn fetch_lyrics(
    path: &Path,
    load_info: &dyn Fn(&Path) -> Result<TrackInfo>,
    api: &dyn LyricsApi,
) -> Result<LyricsResult> {
    let sidecar = path.with_extension("lrc");
    let mut removed_untimed = false;
    if sidecar.exists() {
        let bytes = std::fs::read(&sidecar).with_context(|| format!("read {}", sidecar.display()))?;
        let text = std::str::from_utf8(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes))
            .with_context(|| format!("{} is not valid UTF-8", sidecar.display()))?;
        if has_timestamps(text) {
            return Ok(LyricsResult { status: Status::Existing, removed_untimed });
        }
        std::fs::remove_file(&sidecar)?;
        removed_untimed = true;
    }
    let track = load_info(path)?;
    if track.title.is_empty() || track.artist.is_empty() || track.album.is_empty() {
        bail!("Missing title/artist/album: {}", path.display());
    }
    let status = match find_record(&track, api)? {
        Some(lyrics) => {
            atomic_write(&sidecar, format!("{}\n", lyrics.trim()).as_bytes())?;
            Status::Downloaded
        }
        None => Status::NotFound,
    };
    Ok(LyricsResult { status, removed_untimed })
}

/// Blocking LRCLIB client with the retry policy of the original pipeline.
pub struct Lrclib {
    agent: ureq::Agent,
    base: String,
    backoff: Duration,
}

impl Lrclib {
    pub fn new() -> Self {
        Self::with_base("https://lrclib.net/api", Duration::from_secs(1))
    }

    pub fn with_base(base: &str, backoff: Duration) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .user_agent(concat!("korg/", env!("CARGO_PKG_VERSION"), " (https://github.com/archhaze24/korg)"))
            .build()
            .into();
        Self { agent, base: base.trim_end_matches('/').to_string(), backoff }
    }

    /// Up to three attempts. 404 means "no record"; 429, 5xx and transport errors are retried
    /// with 1 s then 2 s pauses; any other client error fails at once.
    fn request(&self, endpoint: &str, params: &[(&str, String)]) -> Result<Option<Value>> {
        let url = format!("{}/{endpoint}", self.base);
        for attempt in 0..3u32 {
            let mut req = self.agent.get(&url).header("Accept", "application/json");
            for (key, value) in params {
                req = req.query(*key, value);
            }
            match req.call() {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if status == 404 {
                        return Ok(None);
                    }
                    if (200..300).contains(&status) {
                        return response
                            .body_mut()
                            .read_json()
                            .map(Some)
                            .map_err(|e| anyhow!("LRCLIB {endpoint}: bad response: {e}"));
                    }
                    if status != 429 && status < 500 {
                        bail!("LRCLIB {endpoint}: HTTP {status}");
                    }
                    if attempt == 2 {
                        bail!("LRCLIB {endpoint}: HTTP {status}");
                    }
                }
                Err(err) => {
                    if attempt == 2 {
                        return Err(anyhow!("LRCLIB {endpoint}: {err}"));
                    }
                }
            }
            std::thread::sleep(self.backoff * 2u32.pow(attempt));
        }
        unreachable!("the third attempt always returns")
    }
}

impl Default for Lrclib {
    fn default() -> Self {
        Self::new()
    }
}

impl LyricsApi for Lrclib {
    fn get(&self, track: &TrackInfo) -> Result<Option<Value>> {
        self.request(
            "get",
            &[
                ("track_name", track.title.clone()),
                ("artist_name", track.artist.clone()),
                ("album_name", track.album.clone()),
                // Python-style rounding: halves go to the even number.
                ("duration", format!("{}", track.length.round_ties_even() as i64)),
            ],
        )
    }

    fn search(&self, track: &TrackInfo) -> Result<Option<Value>> {
        self.request("search", &[("track_name", track.title.clone()), ("artist_name", track.artist.clone())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    struct Fake {
        get: Option<Value>,
        search: Option<Value>,
        calls: Mutex<Vec<&'static str>>,
    }

    impl Fake {
        fn new(get: Option<Value>, search: Option<Value>) -> Self {
            Self { get, search, calls: Mutex::new(Vec::new()) }
        }
    }

    impl LyricsApi for Fake {
        fn get(&self, _: &TrackInfo) -> Result<Option<Value>> {
            self.calls.lock().unwrap().push("get");
            Ok(self.get.clone())
        }
        fn search(&self, _: &TrackInfo) -> Result<Option<Value>> {
            self.calls.lock().unwrap().push("search");
            Ok(self.search.clone())
        }
    }

    fn info(album: &str) -> TrackInfo {
        TrackInfo { title: "Song".into(), artist: "Artist".into(), album: album.into(), length: 200.0 }
    }

    fn record(extra: Value) -> Value {
        let mut base = json!({"trackName": "Song", "artistName": "Artist", "albumName": "Album",
            "duration": 200, "syncedLyrics": "[00:10.00] test"});
        base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        base
    }

    fn run(path: &Path, track: TrackInfo, api: &dyn LyricsApi) -> Result<LyricsResult> {
        fetch_lyrics(path, &move |_| Ok(track.clone()), api)
    }

    #[test]
    fn plain_text_and_wrong_duration_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("01. Song.flac");
        std::fs::write(path.with_extension("lrc"), "Unsynchronized text").unwrap();
        let plain = record(json!({"syncedLyrics": null, "plainLyrics": "plain"}));
        let api = Fake::new(Some(plain), Some(json!([])));
        let result = run(&path, info("Album"), &api).unwrap();
        assert_eq!(result.status, Status::NotFound);
        assert!(result.removed_untimed);
        assert!(!path.with_extension("lrc").exists());

        let far = record(json!({"duration": 230}));
        let api = Fake::new(Some(far), Some(json!([])));
        assert_eq!(run(&path, info("Album"), &api).unwrap().status, Status::NotFound);
        assert!(!path.with_extension("lrc").exists());
    }

    #[test]
    fn exact_match_is_written_and_then_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("01. Song.flac");
        std::fs::write(path.with_extension("lrc"), "Unsynchronized text").unwrap();
        let api = Fake::new(Some(record(json!({}))), None);
        assert_eq!(run(&path, info("Album"), &api).unwrap().status, Status::Downloaded);
        assert_eq!(api.calls.lock().unwrap().as_slice(), ["get"]);
        assert_eq!(std::fs::read_to_string(path.with_extension("lrc")).unwrap(), "[00:10.00] test\n");

        let again = Fake::new(None, None);
        assert_eq!(run(&path, info("Album"), &again).unwrap().status, Status::Existing);
        assert!(again.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn search_accepts_other_album_but_checks_artist_and_duration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Song.flac");
        let good = record(json!({"albumName": "Original album", "syncedLyrics": "[00:10.00] Correct lyrics"}));
        let candidates = json!([
            record(json!({"artistName": "Other artist"})),
            record(json!({"duration": 210})),
            record(json!({"duration": 201, "syncedLyrics": "[00:10.00] Less close"})),
            good,
        ]);
        let api = Fake::new(None, Some(candidates));
        assert_eq!(run(&path, info("Compilation"), &api).unwrap().status, Status::Downloaded);
        assert_eq!(api.calls.lock().unwrap().as_slice(), ["get", "search"]);
        assert_eq!(std::fs::read_to_string(path.with_extension("lrc")).unwrap(), "[00:10.00] Correct lyrics\n");
    }

    #[test]
    fn album_breaks_ties_and_first_wins_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Song.flac");
        let candidates = json!([
            record(json!({"albumName": "Other", "syncedLyrics": "[00:01.00] first"})),
            record(json!({"albumName": "Album", "syncedLyrics": "[00:01.00] right album"})),
            record(json!({"albumName": "Third", "syncedLyrics": "[00:01.00] third"})),
        ]);
        let api = Fake::new(None, Some(candidates));
        run(&path, info("Album"), &api).unwrap();
        assert_eq!(std::fs::read_to_string(path.with_extension("lrc")).unwrap(), "[00:01.00] right album\n");

        std::fs::remove_file(path.with_extension("lrc")).unwrap();
        let candidates = json!([
            record(json!({"albumName": "A", "syncedLyrics": "[00:01.00] first"})),
            record(json!({"albumName": "B", "syncedLyrics": "[00:01.00] second"})),
        ]);
        run(&path, info("Nowhere"), &Fake::new(None, Some(candidates))).unwrap();
        assert_eq!(std::fs::read_to_string(path.with_extension("lrc")).unwrap(), "[00:01.00] first\n");
    }

    #[test]
    fn bad_durations_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Song.flac");
        let candidates = json!([
            record(json!({"duration": null})),
            record(json!({"duration": "abc"})),
            record(json!({"duration": 0})),
            record(json!({"duration": -3})),
            json!("not an object"),
            record(json!({"duration": "199.5", "syncedLyrics": "[01:02:03] ok"})),
        ]);
        let api = Fake::new(Some(json!({"trackName": "Song"})), Some(candidates));
        assert_eq!(run(&path, info("Album"), &api).unwrap().status, Status::Downloaded);
        assert_eq!(std::fs::read_to_string(path.with_extension("lrc")).unwrap(), "[01:02:03] ok\n");
    }

    #[test]
    fn sidecar_must_be_utf8_and_is_kept_when_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Song.flac");
        std::fs::write(path.with_extension("lrc"), [0xFF, 0xFE, 0x00, 0x41]).unwrap();
        let api = Fake::new(None, None);
        assert!(run(&path, info("Album"), &api).is_err());
        assert!(path.with_extension("lrc").exists());

        std::fs::write(path.with_extension("lrc"), b"\xEF\xBB\xBF[00:01.00] bom").unwrap();
        assert_eq!(run(&path, info("Album"), &api).unwrap().status, Status::Existing);
    }

    #[test]
    fn missing_tags_are_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Song.flac");
        let mut track = info("Album");
        track.artist.clear();
        assert!(run(&path, track, &Fake::new(None, None)).is_err());
    }

    #[test]
    fn normalization_ignores_case_punctuation_and_width() {
        assert_eq!(normalize("Ｓong  — Title!"), normalize("song title"));
        assert_eq!(normalize("Straße"), normalize("STRASSE"));
        assert_ne!(normalize("a b"), normalize("a c"));
    }

    #[test]
    fn timestamps_regex() {
        for ok in ["[00:10.00] x", "[1:05] x", "[12:34:567] x", "[00:10:5]"] {
            assert!(has_timestamps(ok), "{ok}");
        }
        for bad in ["plain", "[ar:Artist]", "[0:5]"] {
            assert!(!has_timestamps(bad), "{bad}");
        }
    }

    /// Serves canned HTTP responses, one per connection, and counts the requests.
    fn serve(statuses: Vec<&'static str>) -> (String, std::thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut served = 0;
            for status in statuses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let body = if status.starts_with("200") { r#"{"ok":true}"# } else { "" };
                let reply =
                    format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(reply.as_bytes()).unwrap();
                served += 1;
            }
            served
        });
        (base, handle)
    }

    #[test]
    fn http_404_is_none_and_not_retried() {
        let (base, server) = serve(vec!["404 Not Found"]);
        let client = Lrclib::with_base(&base, Duration::from_millis(1));
        assert!(client.request("get", &[]).unwrap().is_none());
        assert_eq!(server.join().unwrap(), 1);
    }

    #[test]
    fn http_server_errors_are_retried_then_succeed() {
        let (base, server) = serve(vec!["500 Oops", "429 Slow down", "200 OK"]);
        let client = Lrclib::with_base(&base, Duration::from_millis(1));
        assert_eq!(client.request("get", &[]).unwrap().unwrap()["ok"], true);
        assert_eq!(server.join().unwrap(), 3);
    }

    #[test]
    fn http_gives_up_after_three_attempts() {
        let (base, server) = serve(vec!["503 Down", "503 Down", "503 Down"]);
        let client = Lrclib::with_base(&base, Duration::from_millis(1));
        assert!(client.request("get", &[]).is_err());
        assert_eq!(server.join().unwrap(), 3);
    }

    #[test]
    fn other_client_errors_fail_immediately() {
        let (base, server) = serve(vec!["400 Bad Request"]);
        let client = Lrclib::with_base(&base, Duration::from_millis(1));
        assert!(client.request("get", &[]).is_err());
        assert_eq!(server.join().unwrap(), 1);
    }
}
