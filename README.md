# korg

Kirby's music organizer. Turns a FLAC library tagged with MusicBrainz Picard into a small Opus library.

One `korg run` does four steps:

1. **covers**: resize and clean `cover.jpg`
2. **lyrics**: download synced lyrics from [LRCLIB](https://lrclib.net) as `.lrc` files
3. **encode**: FLAC to Opus (160 kbit/s VBR), MP3 is copied, covers, `.lrc` and booklets are copied along
4. **gain**: ReplayGain for both libraries

Works on Linux, macOS and Windows.

## Install

```sh
brew install archhaze24/tap/korg                                          # macOS, Linux
scoop bucket add archhaze24 https://github.com/archhaze24/scoop-bucket && scoop install korg   # Windows
npx @archhaze24/korg --help                                               # any, via npm
cargo install korg-cli                                                    # from source, Rust 1.89+
```

Prebuilt archives are on the [releases page](https://github.com/archhaze24/korg/releases).

korg calls two external tools, so install them too (Homebrew installs them with the formula):

- [`opusenc`](https://opus-codec.org/downloads/) (opus-tools)
- [`rsgain`](https://github.com/complexlogic/rsgain) with a preset named `opus`

The gain step runs `rsgain easy -p opus`, so create `opus.ini` in rsgain's presets folder:
`~/.config/rsgain/presets/` (Linux), `~/Library/rsgain/presets/` (macOS), `%USERPROFILE%\.rsgain\presets\` (Windows).
This is enough to get R128 tags in Opus files:

```ini
[Opus]
OpusMode=r
```

rsgain ignores `[Opus]` in a folder that mixes Opus and MP3 and writes plain ReplayGain tags there. Use
`[Global]` instead of `[Opus]` if you want R128 everywhere.

`korg doctor` checks the tools and the preset.

## Usage

```sh
korg run                                         # ./toencode -> ./encoded
korg run -s ~/Music/toencode -d ~/Music/encoded
korg run --skip lyrics,gain                      # leave steps out
korg run --dry-run                               # show what would happen
korg covers | lyrics | encode | gain             # one step only
korg restore-names                               # undo renames after a crash
```

Source and destination can also come from `KORG_SOURCE` and `KORG_DEST`. `-j` sets parallel encodes per album
(default: CPU cores minus 2), `-v` prints every checked file.

Don't run two instances on the same folders, and don't edit the source in Picard meanwhile.

## What the steps do

**covers.** `cover.jpg`, `cover.jpeg`, `front.jpg` and `folder.jpg` (any case) become `cover.jpg`. A side over 2000 px
or an EXIF rotation means resize and re-encode (quality 90, progressive); a heavy file is re-encoded only if that
saves 10% or more. Everything else just loses its metadata. Several candidates in one folder: reported, skipped.

**lyrics.** For each FLAC/MP3, LRCLIB is asked by title, artist, album and duration, then by title and artist only.
A match needs the same title and artist (ignoring case and punctuation) and a duration within 2 s; the closest
duration wins, the album only breaks ties. Only lyrics with timestamps are saved. An existing `.lrc` with
timestamps is kept, one without is deleted and searched again. To search again, delete the `.lrc`.

**encode.** Albums run one after another, tracks inside an album in parallel. Tags and artwork are kept; ReplayGain
and encoder tags are dropped, and the FLAC is never modified. A duration that differs by more than 0.1 s is an
error (`--allow-duration-mismatch` makes it a warning). An album is skipped if its folder already exists in the
destination, nothing else is checked: delete the folder to redo it. Covers, `.lrc` and `booklet.pdf` are copied for
every album, skipped ones too.

**gain.** `rsgain easy -m MAX <source>`, then `rsgain easy -m MAX -p opus <dest>`. Both always run.

## Long paths

If `opusenc` says a path was not found, the track is copied to a short temporary path and encoded there. Such
albums are renamed to short names for the rsgain run and renamed back. The renames are journaled; Ctrl+C and
errors restore them. After a crash the next run refuses to start until `korg restore-names`.

## Exit codes

`0` ok, `1` some files failed (see `<dest>/korg-report.json`), `2` bad arguments or setup, `130` interrupted.
Output goes to stderr; `NO_COLOR` is respected.

## Development

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

`tests/e2e.rs` needs `opusenc`, `rsgain`, `flac` and `lame` in `PATH` and skips itself without them.

## License

MIT
