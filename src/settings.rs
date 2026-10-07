//! User settings: a small, hand-readable `key=value` file. Unknown keys and bad lines
//! are ignored, so old and new versions can share a file.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

/// Normal (restored) window rectangle in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub wrap: bool,
    pub status_bar: bool,
    pub window: Option<WindowRect>,
    pub maximized: bool,
    pub last_folder: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeChoice::System,
            wrap: false,
            status_bar: true,
            window: None,
            maximized: false,
            last_folder: None,
        }
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Settings {
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "theme" => {
                    s.theme = match value {
                        "light" => ThemeChoice::Light,
                        "dark" => ThemeChoice::Dark,
                        _ => ThemeChoice::System,
                    }
                }
                "wrap" => s.wrap = parse_bool(value).unwrap_or(s.wrap),
                "status_bar" => s.status_bar = parse_bool(value).unwrap_or(s.status_bar),
                "maximized" => s.maximized = parse_bool(value).unwrap_or(s.maximized),
                "window" => {
                    let n: Vec<i32> = value
                        .split(',')
                        .filter_map(|p| p.trim().parse().ok())
                        .collect();
                    if let [x, y, w, h] = n[..] {
                        if w > 0 && h > 0 {
                            s.window = Some(WindowRect { x, y, w, h });
                        }
                    }
                }
                "last_folder" if !value.is_empty() => s.last_folder = Some(PathBuf::from(value)),
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        let theme = match self.theme {
            ThemeChoice::System => "system",
            ThemeChoice::Light => "light",
            ThemeChoice::Dark => "dark",
        };
        let mut out = format!(
            "# Foxing settings\ntheme={theme}\nwrap={}\nstatus_bar={}\nmaximized={}\n",
            self.wrap, self.status_bar, self.maximized
        );
        if let Some(r) = self.window {
            out.push_str(&format!("window={},{},{},{}\n", r.x, r.y, r.w, r.h));
        }
        if let Some(f) = &self.last_folder {
            out.push_str(&format!("last_folder={}\n", f.display()));
        }
        out
    }

    /// Loads `path`; a missing or unreadable file gives defaults.
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path)
            .map(|t| Settings::parse(&t))
            .unwrap_or_default()
    }

    /// Writes atomically (temp file + rename), creating the folder if needed.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("ini.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(self.to_text().as_bytes())?;
        }
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty_or_missing() {
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(
            Settings::load(Path::new("definitely/not/here.ini")),
            Settings::default()
        );
    }

    #[test]
    fn round_trip() {
        let s = Settings {
            theme: ThemeChoice::Dark,
            wrap: true,
            status_bar: false,
            window: Some(WindowRect {
                x: -10,
                y: 20,
                w: 800,
                h: 600,
            }),
            maximized: true,
            last_folder: Some(PathBuf::from("C:/Users/me/Documents")),
        };
        assert_eq!(Settings::parse(&s.to_text()), s);
    }

    #[test]
    fn tolerates_junk_and_unknown_keys() {
        let s = Settings::parse(
            "# comment\n; also comment\ngarbage line\ntheme = light \nwrap=maybe\nfuture_key=1\nwindow=1,2,0,5\nstatus_bar=no\n",
        );
        assert_eq!(s.theme, ThemeChoice::Light);
        assert!(!s.wrap, "bad bool keeps the default");
        assert_eq!(s.window, None, "zero-size window is rejected");
        assert!(!s.status_bar);
    }

    #[test]
    fn save_creates_folder_and_replaces_atomically() {
        let dir = std::env::temp_dir().join(format!("foxing-settings-{}", std::process::id()));
        let path = dir.join("nested").join("settings.ini");
        let s = Settings {
            wrap: true,
            ..Settings::default()
        };
        s.save(&path).unwrap();
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        assert!(!path.with_extension("ini.tmp").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
