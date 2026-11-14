//! Tag and duration access for the formats the pipeline touches.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::ogg::{OpusFile, tag::VorbisComments};
use lofty::tag::Accessor;

/// Basic facts about a track used for the LRCLIB lookup.
#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub length: f64,
}

/// ReplayGain / R128 keys, in any letter case.
pub fn is_gain_tag(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.starts_with("replaygain_") || lower.starts_with("r128_")
}

/// Keys opusenc adds that are dropped from the result, plus gain tags.
fn is_encoder_tag(key: &str) -> bool {
    key.eq_ignore_ascii_case("encoder") || key.eq_ignore_ascii_case("encoder_options") || is_gain_tag(key)
}

pub fn read_info(path: &Path) -> Result<TrackInfo> {
    let file = lofty::read_from_path(path).with_context(|| format!("read tags of {}", path.display()))?;
    let tag = file.primary_tag().or_else(|| file.first_tag()).ok_or_else(|| anyhow!("no tags: {}", path.display()))?;
    Ok(TrackInfo {
        title: tag.title().map(|v| v.into_owned()).unwrap_or_default(),
        artist: tag.artist().map(|v| v.into_owned()).unwrap_or_default(),
        album: tag.album().map(|v| v.into_owned()).unwrap_or_default(),
        length: file.properties().duration().as_secs_f64(),
    })
}

/// Duration in seconds as the container reports it.
pub fn duration(path: &Path) -> Result<f64> {
    let file = lofty::read_from_path(path).with_context(|| format!("read {}", path.display()))?;
    Ok(file.properties().duration().as_secs_f64())
}

/// Removes matching comments while keeping the order and case of everything else.
fn remove_comments(tag: &mut VorbisComments, remove: impl Fn(&str) -> bool) -> bool {
    if !tag.items().any(|(key, _)| remove(key)) {
        return false;
    }
    let items: Vec<_> = tag.take_items().collect();
    for (key, value) in items {
        if !remove(&key) {
            tag.push(key, value);
        }
    }
    true
}

pub fn flac_has_gain_tags(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let flac =
        FlacFile::read_from(&mut file, ParseOptions::new()).with_context(|| format!("read {}", path.display()))?;
    Ok(flac.vorbis_comments().is_some_and(|tag| tag.items().any(|(key, _)| is_gain_tag(key))))
}

/// Deletes ReplayGain tags from a FLAC file in place.
pub fn strip_flac_gain_tags(path: &Path) -> Result<()> {
    let mut file = File::open(path)?;
    let mut flac = FlacFile::read_from(&mut file, ParseOptions::new())?;
    drop(file);
    if let Some(tag) = flac.vorbis_comments_mut() {
        if remove_comments(tag, is_gain_tag) {
            flac.save_to_path(path, WriteOptions::default())?;
        }
    }
    Ok(())
}

/// Deletes encoder and gain tags from a freshly encoded Opus file.
pub fn clean_opus_tags(path: &Path) -> Result<()> {
    let mut file = File::open(path)?;
    let mut opus = OpusFile::read_from(&mut file, ParseOptions::new())?;
    drop(file);
    if remove_comments(opus.vorbis_comments_mut(), is_encoder_tag) {
        opus.save_to_path(path, WriteOptions::default())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_tag_matching_ignores_case() {
        assert!(is_gain_tag("REPLAYGAIN_TRACK_GAIN"));
        assert!(is_gain_tag("replaygain_album_peak"));
        assert!(is_gain_tag("R128_TRACK_GAIN"));
        assert!(!is_gain_tag("ARTIST"));
        assert!(!is_gain_tag("MY_REPLAYGAIN_NOTE"));
    }

    #[test]
    fn remove_comments_keeps_order_and_duplicates() {
        let mut tag = VorbisComments::new();
        for (k, v) in
            [("TITLE", "A"), ("artist", "X"), ("ENCODER", "libopus"), ("ARTIST", "Y"), ("R128_TRACK_GAIN", "1")]
        {
            tag.push(k.to_string(), v.to_string());
        }
        assert!(remove_comments(&mut tag, is_encoder_tag));
        let left: Vec<_> = tag.items().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        assert_eq!(
            left,
            vec![("TITLE".into(), "A".into()), ("artist".into(), "X".into()), ("ARTIST".into(), "Y".into())]
        );
        assert!(!remove_comments(&mut tag, is_encoder_tag));
    }
}
