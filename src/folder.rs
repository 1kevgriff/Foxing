//! A folder of text files: listing and naming new files. Platform-neutral.

use std::io;
use std::path::{Path, PathBuf};

/// Extensions shown in the folder list (compared case-insensitively).
pub const EXTENSIONS: [&str; 2] = ["txt", "md"];

pub fn is_text_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Text files directly inside `dir` (no subfolders), sorted by name, case-insensitively.
pub fn list(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| is_text_file(p))
        .collect();
    files.sort_by_cached_key(|p| file_name(p).to_lowercase());
    Ok(files)
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A name for a new file in `dir` that doesn't exist yet: "Untitled.txt",
/// "Untitled 2.txt", ...
pub fn new_file_path(dir: &Path) -> PathBuf {
    let first = dir.join("Untitled.txt");
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("Untitled {n}.txt")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("foxing-folder-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn lists_text_files_sorted_flat() {
        let d = temp_dir("list");
        for f in ["b.md", "A.TXT", "c.txt", "image.png", "noext"] {
            std::fs::write(d.join(f), "x").unwrap();
        }
        std::fs::create_dir(d.join("sub.txt")).unwrap();
        std::fs::write(d.join("sub.txt").join("inner.txt"), "x").unwrap();
        let names: Vec<String> = list(&d).unwrap().iter().map(|p| file_name(p)).collect();
        assert_eq!(names, ["A.TXT", "b.md", "c.txt"]);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn missing_folder_is_an_error() {
        assert!(list(Path::new("definitely/not/a/folder")).is_err());
    }

    #[test]
    fn new_file_names_skip_existing() {
        let d = temp_dir("new");
        assert_eq!(new_file_path(&d), d.join("Untitled.txt"));
        std::fs::write(d.join("Untitled.txt"), "").unwrap();
        std::fs::write(d.join("Untitled 2.txt"), "").unwrap();
        assert_eq!(new_file_path(&d), d.join("Untitled 3.txt"));
        std::fs::remove_dir_all(&d).ok();
    }
}
