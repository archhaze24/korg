# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/). The release workflow publishes the section of the
tag as the GitHub release notes.

## [Unreleased]

### Changed

- An LRCLIB request that gets no answer within 8 seconds now fails at once, without retries (it used to wait 20 seconds and report a confusing JSON error). Change the limit with `--lyrics-timeout SECS`. The next run looks the track up again.

## [0.1.0] - 2026-11-14

### Added

- First version: `korg run` (covers, synced lyrics, FLAC to Opus, ReplayGain) and the single-step commands `covers`, `lyrics`, `encode` and `gain`, plus `doctor`, `restore-names` and `--dry-run`.
- Colored, progress-bar output that falls back to plain lines when it is not a terminal; `NO_COLOR` is honored.
