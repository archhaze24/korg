# Releasing korg

A release has three parts that go out in this order: **GitHub release** (binaries, built by CI from a tag),
**crates.io** (`cargo publish`), **npm** (wrapper packages assembled from the GitHub release assets, so
`npx @archhaze24/korg` works). Afterwards push the Homebrew and Scoop files (step 6).

Nothing here runs by itself except the `Release` workflow on a tag push. Do it from a machine where you are
logged in to crates.io and npm. Replace `X.Y.Z` with the new version everywhere. For a prerelease use a SemVer
prerelease version (e.g. `0.2.0-rc.1`, tag `v0.2.0-rc.1`).

Package names: crates.io `korg-cli` (installs the binary `korg`; the plain name `korg` is taken), npm
`@archhaze24/korg` plus platform packages `@archhaze24/korg-<os>-<arch>[-musl]` (the unscoped `korg` is taken),
Homebrew `archhaze24/tap/korg`, Scoop `korg` from `archhaze24/scoop-bucket`.

The repository is private and Actions minutes are limited (GitHub Free): the `CI` workflow runs only when
started by hand (`gh workflow run ci.yml`). Run it once before tagging, and run the manual `Release`
workflow as the dry run in step 1.8. Both are free after the repository goes public.

## 0. Prerequisites (once)

- `cargo login` with a crates.io API token (scopes `publish-new` + `publish-update`).
- `npm login` as `archhaze24` (2FA prompts for an OTP on every `npm publish`). All eight packages are new in the
  first release; the scope `@archhaze24` already exists.
- Node >= 18, `tar` (bsdtar on macOS is fine), `gh` (optional).
- The repos `archhaze24/homebrew-tap` (`Formula/`) and `archhaze24/scoop-bucket` (`bucket/`) already exist.
- The repository must be public before users can download release assets, `brew install` or `scoop install`
  (the formula and manifest point at the public release URLs). Make it public only when you decide to.

## 1. Prepare

1. Get the release commit onto `main` and start the `CI` workflow by hand; wait until it is green.
2. Bump `version` in `Cargo.toml` (and `Cargo.lock` follows with `cargo check`).
3. Finish `CHANGELOG.md`: rename `## [Unreleased]` notes into `## [X.Y.Z] - <date>` and keep an empty
   `## [Unreleased]` heading above it. Preview with `.github/scripts/release-notes.sh vX.Y.Z`.
4. Check the "Install" section of `README.md`: it must list only channels that work after the release.
5. `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
6. `cargo publish --dry-run` (must succeed) and `cargo package --list` (no stray files).
7. Commit (`chore(release): X.Y.Z`), push `main`.
8. Optional safety net: run the release workflow manually. It builds all seven targets and checks the
   changelog, but never creates a release: `gh workflow run release.yml --ref main`. The tag/version check is
   skipped on manual runs.

## 2. GitHub release (binaries)

```
git tag -s vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z
```

The `Release` workflow first checks that the tag (without the leading `v`) equals the version in `Cargo.toml`
and that `CHANGELOG.md` has notes for it, then builds x86_64 Linux (glibc and static musl), aarch64 Linux
(glibc and static musl), x86_64/aarch64 macOS and x86_64 Windows, and creates the GitHub release `vX.Y.Z`. A tag
containing `-` is marked as a prerelease. Wait until it is green and check that the release has 14 files
(7 archives + 7 `.sha256`), each archive holding `korg[.exe]`, `LICENSE` and `README.md`. The same run renders
`korg.rb` (Homebrew) and `korg.json` (Scoop) into the workflow artifact `packaging` (not a release asset).

If a build fails, fix it, delete the tag (`git push origin :refs/tags/vX.Y.Z && git tag -d vX.Y.Z`), and tag
again. Nothing is published to crates.io/npm before this step succeeds.

## 3. crates.io

```
cargo publish
```

A published version cannot be overwritten; use `cargo yank --version X.Y.Z korg-cli` if something is badly wrong.

## 4. npm

The launcher package `@archhaze24/korg` uses `npm/launcher.js` and depends on seven platform packages
(`@archhaze24/korg-linux-x64`, `-linux-x64-musl`, `-linux-arm64`, `-linux-arm64-musl`, `-darwin-x64`,
`-darwin-arm64`, `-win32-x64`) that carry the binary in `bin/`; npm installs only the one matching the machine.

```
node npm/build.mjs vX.Y.Z            # downloads the release assets, writes npm/dist/*
cd npm/dist
for p in korg-linux-x64 korg-linux-x64-musl korg-linux-arm64 korg-linux-arm64-musl korg-darwin-x64 korg-darwin-arm64 korg-win32-x64; do
  (cd "$p" && npm publish --dry-run --access public)   # look at the file list, then repeat without --dry-run
done
(cd korg && npm publish --dry-run --access public)     # launcher LAST, otherwise installs briefly fail
```

Publish the seven platform packages first, then the launcher, all with `npm publish --access public` (scoped
packages need it). For a prerelease version (contains `-`) add `--tag next` to every `npm publish`, otherwise it
becomes `latest`.

`build.mjs` wipes its `--out` directory: it refuses the repo root, its ancestors, the home dir and `/`, and
refuses a non-empty directory that lacks the `.korg-npm-dist` marker it writes. Every archive is checked against
its `.sha256` release asset. After 72 hours a version can no longer be unpublished; use
`npm deprecate @archhaze24/korg@X.Y.Z "reason"` instead.

## 5. Verify

```
npx @archhaze24/korg@X.Y.Z --version
cargo install korg-cli --version X.Y.Z --force && korg --version
```

Then bump `main` to the next development version when you start the next cycle.

## 6. Homebrew and Scoop

Download the workflow artifact `packaging` of the release run (`gh run download <run-id> -n packaging`; or render
it yourself with `.github/scripts/render-packaging.sh vX.Y.Z`, which downloads the public `.sha256` files). Copy
`korg.rb` to `Formula/korg.rb` in `archhaze24/homebrew-tap` and `korg.json` to `bucket/korg.json` in
`archhaze24/scoop-bucket`, commit and push. Check: `brew install archhaze24/tap/korg && korg --version`,
`scoop bucket add archhaze24 https://github.com/archhaze24/scoop-bucket && scoop install korg`. Skip this for
prereleases. Templates live in `packaging/`. The formula depends on the Homebrew `opus-tools` and `rsgain`;
the Scoop manifest only prints a note, because korg also needs an rsgain preset named `opus` (see the README).

## Notes

- The macOS binaries are unsigned and not notarized; a downloaded archive may need
  `xattr -d com.apple.quarantine korg`. Installing via npm or cargo does not trigger Gatekeeper.
- The `linux-*-musl` packages carry a fully static binary; the launcher detects musl at runtime and prefers
  them there, and falls back to them when the glibc package is missing.
- `npm/build.mjs --assets <dir>` builds from already-downloaded archives instead of fetching them.
