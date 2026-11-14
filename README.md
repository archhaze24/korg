# korg

Kirby's music organizer. One binary that turns a tagged FLAC library into a compact Opus library.

You keep your masters in a source folder (tagged with MusicBrainz Picard); `korg` builds the portable copy:

1. **covers**: cleans `cover.jpg` (resize, strip metadata)
2. **lyrics**: downloads synced lyrics from [LRCLIB](https://lrclib.net) as `.lrc` files
3. **encode**: FLAC to Opus (160 kbit/s VBR) with tags and artwork kept; MP3 is copied; covers, `.lrc` and booklets are copied along
4. **gain**: ReplayGain for both libraries with `rsgain`

Runs on Linux, macOS and Windows.

## Requirements

- [`opusenc`](https://opus-codec.org/downloads/) (opus-tools) for step 3
- [`rsgain`](https://github.com/complexlogic/rsgain) for step 4, with a preset named `opus`

```sh
korg doctor   # checks both tools and the preset
```

`korg gain` runs `rsgain easy -m MAX -p opus <dest>`, so rsgain needs a preset file `opus.ini` in its presets
folder (for example `~/.config/rsgain/presets/` on Linux and macOS). rsgain does not ship one. A minimal preset
that writes R128 tags for Opus:

```ini
[Global]
TagMode=i
Album=true
TargetLoudness=-18
ClipMode=p
OpusMode=s
```

## Usage

```sh
korg run                                  # all steps, ./toencode -> ./encoded
korg run -s ~/Music/toencode -d ~/Music/encoded
korg run --skip lyrics,gain               # leave steps out
korg run --dry-run                        # show what would happen
korg covers | lyrics | encode | gain      # one step only
korg restore-names                        # see "Interrupted runs"
```

`--source`, `--dest`, `--opusenc` and `--rsgain` can also be set with `KORG_SOURCE`, `KORG_DEST`,
`KORG_OPUSENC` and `KORG_RSGAIN`. `-j/--jobs` sets parallel encodes per album (default: CPU cores minus 2),
`-v/--verbose` prints every checked file.

The source and destination libraries must not contain each other. Do not run two instances on the same folders,
and do not edit the source library in Picard while it runs.

## What each step does

**covers.** Looks in every folder of the source library for `cover.jpg`, `cover.jpeg`, `front.jpg` or
`folder.jpg` (any case) and stores the result as `cover.jpg`. Images with a side over 2000 px or a file over
1.5 MiB are resized to 2000 px and re-encoded (quality 90, progressive). Smaller ones only lose their metadata
segments; pixels are not recompressed. A folder with several candidates is reported as an error and left alone.

**lyrics.** For every FLAC/MP3, looks up LRCLIB: first by title, artist, album and duration, then by title and
artist only. A record is accepted when title and artist match (ignoring case, punctuation and Unicode variants)
and the duration is within 2 s. The closest duration wins, the album only breaks ties, so tracks from
compilations still match. Only lyrics with timestamps are saved, next to the track with the same name and
the `.lrc` extension. Existing timestamped `.lrc` files are kept; an `.lrc` without timestamps is deleted and
searched again; an unreadable (non-UTF-8) one is reported and kept. To search again for a track, delete its `.lrc`.

**encode.** Albums (folders with FLAC/MP3) are processed one after another, tracks inside an album in parallel.
The Opus file keeps the source tags and artwork; `encoder`/`encoder_options` and ReplayGain tags are dropped.
Stale ReplayGain tags in a FLAC never reach the encoder, and the FLAC itself stays untouched. An output whose
duration differs from the source by more than 0.1 s is an error (`--allow-duration-mismatch` turns it into a warning).
An album is **skipped when its folder already exists in the destination**: nothing else is checked. To redo an
album, delete its destination folder. Tracks that sit directly in the source root are skipped when the
destination root already has `.opus`/`.mp3` files. Afterwards `cover.jpg`, `booklet.pdf` and `.lrc` files are
copied for every album, skipped ones included (also with `--skip encode`).

**gain.** Runs `rsgain easy -m MAX <source>` and `rsgain easy -m MAX -p opus <dest>`. Both always run, even if
the first fails. rsgain output is shown as is.

### Long paths on Windows

`opusenc` is first called on the real path. Only if it reports that the file or path was not found, the track
is copied to a short temporary path and encoded there. For such albums the folders are temporarily renamed to
short names during ReplayGain (both libraries, album stays together) and renamed back at the end.

### Interrupted runs

Every rename is written to a journal (`renames.json` in the user state directory) before it happens.
Normal errors and Ctrl+C restore the names automatically. After a crash or power loss the next run refuses to
start until you run `korg restore-names`. It stops on conflicts and never overwrites foreign files.

## Output

Progress goes to stderr. Colors follow `NO_COLOR` and are off when output is not a terminal.
Exit code: `0` success, `1` some files failed (details in `<dest>/korg-report.json`, deleted after a clean run),
`2` bad arguments or setup, `130` interrupted.

## Development

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

`tests/e2e.rs` runs the whole pipeline on generated files; it needs `opusenc`, `rsgain`, `flac` and `lame` in
`PATH` (otherwise it skips itself) and does not use the network.

## License

MIT
