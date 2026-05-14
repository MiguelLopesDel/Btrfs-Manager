use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CompareError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareKind {
    Created,
    Removed,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareEntry {
    pub path: PathBuf,
    pub kind: CompareKind,
    pub old_len: Option<u64>,
    pub new_len: Option<u64>,
}

pub fn compare_dirs_shallow(
    old_root: &Path,
    new_root: &Path,
) -> Result<Vec<CompareEntry>, CompareError> {
    let old = collect_files(old_root, old_root)?;
    let new = collect_files(new_root, new_root)?;
    let keys: BTreeSet<_> = old.keys().chain(new.keys()).cloned().collect();
    let mut entries = Vec::new();

    for key in keys {
        match (old.get(&key), new.get(&key)) {
            (Some(old_len), Some(new_len)) if old_len != new_len => entries.push(CompareEntry {
                path: key,
                kind: CompareKind::Modified,
                old_len: Some(*old_len),
                new_len: Some(*new_len),
            }),
            (Some(old_len), None) => entries.push(CompareEntry {
                path: key,
                kind: CompareKind::Removed,
                old_len: Some(*old_len),
                new_len: None,
            }),
            (None, Some(new_len)) => entries.push(CompareEntry {
                path: key,
                kind: CompareKind::Created,
                old_len: None,
                new_len: Some(*new_len),
            }),
            _ => {}
        }
    }

    Ok(entries)
}

fn collect_files(root: &Path, dir: &Path) -> Result<BTreeMap<PathBuf, u64>, CompareError> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            files.extend(collect_files(root, &path)?);
        } else if metadata.is_file() {
            let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            files.insert(relative, metadata.len());
        }
    }
    Ok(files)
}
