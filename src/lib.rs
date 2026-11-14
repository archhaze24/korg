//! korg: Kirby's music organizer.
//!
//! The pipeline turns a tagged FLAC library into an Opus library: cover cleanup, synced
//! lyrics from LRCLIB, `opusenc` encoding and `rsgain` ReplayGain.

pub mod audio;
pub mod cli;
pub mod cover;
pub mod encode;
pub mod fsutil;
pub mod gain;
pub mod lyrics;
pub mod pipeline;
pub mod tools;
pub mod ui;
