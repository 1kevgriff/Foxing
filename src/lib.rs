//! Platform-neutral core: text engine, editing model, and UI components.
//! No OS-specific types belong here; each platform renders and forwards input.

pub mod buffer;
pub mod document;
pub mod editor;
pub mod folder;
pub mod settings;
pub mod text;
pub mod textunits;
pub mod ui;
