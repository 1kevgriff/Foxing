//! The open document: file path, unsaved-changes state, load and save.
//! Contents live in a [`Buffer`]; bytes round-trip unchanged, line endings included.

use crate::buffer::Buffer;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "Foxing";

#[cfg(windows)]
pub const NATIVE_EOL: &str = "\r\n";
#[cfg(not(windows))]
pub const NATIVE_EOL: &str = "\n";

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
    pub fn open(path: PathBuf) -> io::Result<(Document, Buffer)> {
        let body = match Buffer::open(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Buffer::new(),
            Err(e) => return Err(e),
        };
        let doc = Document {
            path: Some(path),
            dirty: false,
        };
        Ok((doc, body))
    }

    /// Writes `body` as UTF-8 to a temp file beside `path`, then renames it over `path`,
    /// so a failed save never leaves a truncated file. Then adopts `path`.
    pub fn save_as(&mut self, path: PathBuf, body: &Buffer) -> io::Result<()> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let tmp = path.with_file_name(format!(".{}.foxing-tmp", name.unwrap_or_default()));
        let write = || -> io::Result<()> {
            let mut w = io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp)?);
            body.write_to(&mut w)?;
            w.flush()?;
            w.get_ref().sync_all()?;
            drop(w);
            std::fs::rename(&tmp, &path)
        };
        if let Err(e) = write() {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
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
    fn open_keeps_bytes() {
        let p = tmp("open.txt");
        std::fs::write(&p, "a\r\nb\rc\n").unwrap();
        let (d, body) = Document::open(p).unwrap();
        assert_eq!(body.slice(0..body.len()), "a\r\nb\rc\n");
        assert_eq!(d.title(), "open.txt - Foxing");
    }

    #[test]
    fn open_missing_file_is_empty() {
        let p = tmp("missing.txt");
        let _ = std::fs::remove_file(&p);
        let (d, body) = Document::open(p.clone()).unwrap();
        assert!(body.is_empty());
        assert_eq!(d.path(), Some(p.as_path()));
    }

    #[test]
    fn save_writes_bytes_and_cleans() {
        let p = tmp("save.txt");
        std::fs::write(&p, "old contents").unwrap();
        let mut d = Document::new();
        d.mark_dirty();
        d.save_as(p.clone(), &Buffer::from_text("x\r\ny ✓"))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "x\r\ny ✓");
        assert!(!p.with_file_name(".save.txt.foxing-tmp").exists());
        assert!(!d.is_dirty());
        assert_eq!(d.name(), "save.txt");
    }
}
