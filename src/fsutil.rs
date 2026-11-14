//! Small filesystem helpers shared by every step.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

/// Writes `data` to `path` through a temporary file in the same directory, so readers
/// never see a half-written file and an existing file is replaced in one step.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".korg-")
        .tempfile_in(parent)
        .with_context(|| format!("create temp file in {}", parent.display()))?;
    tmp.write_all(data)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// Copies a file, creating missing parent directories of the destination.
pub fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::copy(src, dst).with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
    Ok(())
}

/// Absolute, symlink-free path that also works for locations that do not exist yet:
/// the nearest existing ancestor is canonicalized and the remaining parts appended.
/// On Windows `dunce` avoids the `\\?\` prefix that external tools may reject.
pub fn resolve_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    // Fold `.` and `..` lexically first so the existence walk below only sees plain names.
    let mut existing = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::ParentDir => {
                existing.pop();
            }
            Component::CurDir => {}
            other => existing.push(other),
        }
    }
    let mut tail = Vec::new();
    while !existing.exists() {
        match existing.file_name() {
            Some(name) => tail.push(name.to_owned()),
            None => break,
        }
        existing.pop();
    }
    let mut resolved = dunce::canonicalize(&existing).with_context(|| format!("resolve {}", existing.display()))?;
    resolved.extend(tail.iter().rev());
    Ok(resolved)
}

/// True when one path is the other or lies inside it.
pub fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_handles_missing_tail() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let resolved = resolve_path(&root.join("a").join("b")).unwrap();
        assert_eq!(resolved, root.join("a").join("b"));
        let dotted = resolve_path(&root.join("a").join("..").join("c")).unwrap();
        assert_eq!(dotted, root.join("c"));
    }

    #[test]
    fn overlap_detects_nesting() {
        assert!(overlaps(Path::new("/a/b"), Path::new("/a")));
        assert!(overlaps(Path::new("/a"), Path::new("/a/b")));
        assert!(overlaps(Path::new("/a"), Path::new("/a")));
        assert!(!overlaps(Path::new("/a/b"), Path::new("/a/c")));
        assert!(!overlaps(Path::new("/ab"), Path::new("/a")));
    }

    #[test]
    fn atomic_write_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x/y.txt");
        atomic_write(&file, b"one").unwrap();
        atomic_write(&file, b"two").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"two");
        let leftovers: Vec<_> = fs::read_dir(file.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
    }
}
