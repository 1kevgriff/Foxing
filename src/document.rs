//! The open document: file path, unsaved-changes state, load and save.
//! Text crossing this API uses `\n` line endings; shells convert at their boundary.

use crate::text;
use std::io;
use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "Foxing";

#[cfg(windows)]
const NATIVE_EOL: &str = "\r\n";
#[cfg(not(windows))]
const NATIVE_EOL: &str = "\n";

#[derive(Debug, Default)]
pub struct Document {
    path: Option<PathBuf>,
    dirty: bool,
}

impl Document {
    /// An untitled, unmodified document.
    pub const fn new() -> Self {
        Document {
            path: None,
            dirty: false,
        }
    }

    /// Opens `path`. A missing file yields an empty document created on first save.
    pub fn open(path: PathBuf) -> io::Result<(Document, String)> {
        let body = match std::fs::read(&path) {
            Ok(bytes) => text::to_lf(&text::decode(&bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let doc = Document {
            path: Some(path),
            dirty: false,
        };
        Ok((doc, body))
    }

    /// Writes `body` as UTF-8 with native line endings, then adopts `path`.
    pub fn save_as(&mut self, path: PathBuf, body: &str) -> io::Result<()> {
        std::fs::write(&path, text::with_eol(body, NATIVE_EOL))?;
        self.path = Some(path);
        self.dirty = false;
        Ok(())
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Marks unsaved changes. Returns true if the document was clean before.
    pub fn mark_dirty(&mut self) -> bool {
        !std::mem::replace(&mut self.dirty, true)
    }

    pub fn name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into())
    }

    pub fn title(&self) -> String {
        let star = if self.dirty { "*" } else { "" };
        format!("{star}{} - {APP_NAME}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("foxing-doc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn new_is_untitled_and_clean() {
        let d = Document::new();
        assert_eq!(d.title(), "Untitled - Foxing");
        assert!(!d.is_dirty());
        assert_eq!(d.path(), None);
    }

    #[test]
    fn mark_dirty_reports_transition_once() {
        let mut d = Document::new();
        assert!(d.mark_dirty());
        assert!(!d.mark_dirty());
        assert_eq!(d.title(), "*Untitled - Foxing");
    }

    #[test]
    fn open_normalizes_to_lf() {
        let p = tmp("open.txt");
        std::fs::write(&p, "a\r\nb\rc\n").unwrap();
        let (d, body) = Document::open(p).unwrap();
        assert_eq!(body, "a\nb\nc\n");
        assert_eq!(d.title(), "open.txt - Foxing");
    }

    #[test]
    fn open_missing_file_is_empty() {
        let p = tmp("missing.txt");
        let _ = std::fs::remove_file(&p);
        let (d, body) = Document::open(p.clone()).unwrap();
        assert_eq!(body, "");
        assert_eq!(d.path(), Some(p.as_path()));
    }

    #[test]
    fn save_writes_native_eol_and_cleans() {
        let p = tmp("save.txt");
        let mut d = Document::new();
        d.mark_dirty();
        d.save_as(p.clone(), "x\ny ✓").unwrap();
        let expected = format!("x{NATIVE_EOL}y ✓");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), expected);
        assert!(!d.is_dirty());
        assert_eq!(d.name(), "save.txt");
    }
}
