//! Step 4: ReplayGain through `rsgain`, plus the temporary path shortening some albums need.
//!
//! Long album paths that only encoded through a short-path retry would trip `rsgain` too.
//! Those albums are renamed to short names for the scan and renamed back afterwards. Every
//! rename is journaled first, so an interrupted run can be undone with `korg restore-names`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::fsutil::atomic_write;

const AUDIO_EXTENSIONS: [&str; 3] = ["flac", "mp3", "opus"];

/// Location of the rename journal: one fixed file per user, so `restore-names` needs no arguments.
pub fn journal_path() -> PathBuf {
    let base = dirs::state_dir().or_else(dirs::data_local_dir).unwrap_or_else(std::env::temp_dir);
    base.join("korg").join("renames.json")
}

type Operations = Vec<[String; 2]>;

fn save(journal: &Path, operations: &Operations) -> Result<()> {
    atomic_write(journal, &serde_json::to_vec_pretty(operations)?)
}

fn short_name() -> String {
    Uuid::new_v4().simple().to_string()[..12].to_string()
}

/// Gives the affected albums short directory and file names in both libraries.
///
/// `affected` holds album paths relative to the library roots. Albums stay together
/// (including MP3s and sidecars) so album gain is still computed across the whole album.
pub fn shorten_for_replaygain(source: &Path, dest: &Path, affected: &BTreeSet<PathBuf>, journal: &Path) -> Result<()> {
    let mut operations = Operations::new();
    save(journal, &operations)?;
    let mut movement = |old: &Path, new: &Path| -> Result<()> {
        if new.exists() {
            bail!("{} already exists", new.display());
        }
        operations.push([old.display().to_string(), new.display().to_string()]);
        save(journal, &operations)?;
        fs::rename(old, new).with_context(|| format!("rename {} -> {}", old.display(), new.display()))
    };
    // Deepest first, in case folders with tracks are nested.
    let mut albums: Vec<&PathBuf> = affected.iter().collect();
    albums.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for relative in albums {
        for root in [source, dest] {
            let album = root.join(relative);
            if !album.is_dir() {
                continue;
            }
            let short_album = if album == root {
                album
            } else {
                let short = root.join(format!(".rg-{}", short_name()));
                movement(&album, &short)?;
                short
            };
            let mut files: Vec<PathBuf> = fs::read_dir(&short_album)?
                .collect::<std::io::Result<Vec<_>>>()?
                .into_iter()
                .map(|e| e.path())
                .collect();
            files.sort();
            for track in files {
                let extension = track.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                if track.is_file() && AUDIO_EXTENSIONS.contains(&extension.as_str()) {
                    movement(&track, &short_album.join(format!("{}.{extension}", short_name())))?;
                }
            }
        }
    }
    Ok(())
}

/// Undoes the journaled renames in reverse order and removes the journal.
/// A collision stops the process and keeps the journal; foreign files are never overwritten.
pub fn restore_names(journal: &Path) -> Result<()> {
    let mut operations: Operations =
        serde_json::from_slice(&fs::read(journal)?).with_context(|| format!("parse {}", journal.display()))?;
    while let Some([old, new]) = operations.last().cloned() {
        let (old_path, new_path) = (Path::new(&old), Path::new(&new));
        if new_path.exists() {
            if old_path.exists() {
                bail!("Restore collision: {old}; journal: {}", journal.display());
            }
            fs::rename(new_path, old_path).with_context(|| format!("restore {old}"))?;
        } else if !old_path.exists() {
            bail!("Cannot restore missing path: {new}; journal: {}", journal.display());
        }
        operations.pop();
        save(journal, &operations)?;
    }
    fs::remove_file(journal)?;
    Ok(())
}

/// Arguments for the two ReplayGain scans: the source library, then the Opus library.
pub fn rsgain_commands(source: &Path, dest: &Path) -> [Vec<String>; 2] {
    let base = |extra: &[&str], root: &Path| -> Vec<String> {
        ["easy", "-m", "MAX"]
            .into_iter()
            .chain(extra.iter().copied())
            .map(String::from)
            .chain([root.display().to_string()])
            .collect()
    };
    [base(&[], source), base(&["-p", "opus"], dest)]
}

/// Runs one `rsgain` scan with its output going straight to the terminal.
pub fn run_rsgain(rsgain: &Path, args: &[String]) -> Result<()> {
    let status = Command::new(rsgain).args(args).status().with_context(|| format!("start {}", rsgain.display()))?;
    if !status.success() {
        bail!("rsgain exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_restore_without_changing_album_members() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (src, dst) = (root.join("src"), root.join("dst"));
        let relative = PathBuf::from("artist/album");
        let mut expected = Vec::new();
        for library in [&src, &dst] {
            let album = library.join(&relative);
            fs::create_dir_all(&album).unwrap();
            for name in ["long track.flac", "another.mp3", "cover.jpg", "long track.lrc"] {
                let path = album.join(name);
                fs::write(&path, name).unwrap();
                expected.push((path, name));
            }
        }
        let journal = root.join("renames.json");
        shorten_for_replaygain(&src, &dst, &BTreeSet::from([relative.clone()]), &journal).unwrap();
        for library in [&src, &dst] {
            assert!(!library.join(&relative).exists());
            let staged: Vec<_> = fs::read_dir(library)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(".rg-"))
                .collect();
            assert_eq!(staged.len(), 1);
            let count = |ext: &str| {
                fs::read_dir(&staged[0])
                    .unwrap()
                    .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == ext))
                    .count()
            };
            assert_eq!((count("flac"), count("mp3"), count("jpg"), count("lrc")), (1, 1, 1, 1));
        }
        restore_names(&journal).unwrap();
        assert!(!journal.exists());
        for (path, content) in expected {
            assert_eq!(fs::read_to_string(path).unwrap(), content);
        }
    }

    #[test]
    fn restore_stops_on_collision_and_keeps_journal() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("renames.json");
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        fs::write(&old, "foreign").unwrap();
        fs::write(&new, "ours").unwrap();
        save(&journal, &vec![[old.display().to_string(), new.display().to_string()]]).unwrap();
        assert!(restore_names(&journal).is_err());
        assert!(journal.exists());
        assert_eq!(fs::read_to_string(&old).unwrap(), "foreign");
        assert_eq!(fs::read_to_string(&new).unwrap(), "ours");
    }

    #[test]
    fn root_level_album_is_not_moved() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("src"), dir.path().join("dst"));
        for library in [&src, &dst] {
            fs::create_dir_all(library).unwrap();
            fs::write(library.join("a.flac"), "x").unwrap();
        }
        let journal = dir.path().join("j.json");
        shorten_for_replaygain(&src, &dst, &BTreeSet::from([PathBuf::new()]), &journal).unwrap();
        assert!(src.is_dir() && dst.is_dir());
        assert!(!src.join("a.flac").exists());
        restore_names(&journal).unwrap();
        assert!(src.join("a.flac").exists() && dst.join("a.flac").exists());
    }

    #[test]
    fn rsgain_arguments_match_the_two_scans() {
        let [first, second] = rsgain_commands(Path::new("/s"), Path::new("/d"));
        assert_eq!(first, ["easy", "-m", "MAX", "/s"]);
        assert_eq!(second, ["easy", "-m", "MAX", "-p", "opus", "/d"]);
    }
}
