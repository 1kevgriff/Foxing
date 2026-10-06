# Foxing

A tiny plain-text editor for Windows, written in Rust. Just notes.

![Foxing editing a short list of notes](docs/screenshot.png)

Foxing is the brown spotting that shows up on old paper. This is the
paper.

## Why

- **Small.** About 256 KB, a single exe with no runtime to install.
- **Fast.** Raw Win32 and the native edit control. No framework, no GPU
  startup.
- **Plain.** Opens UTF-8, UTF-16, and legacy Windows-1252 files. Saves
  UTF-8.

## Features

- New, Open, Save, Save As
- Find (`Ctrl+F`) and Find Next (`F3`)
- Word wrap (Format menu)
- Open from the command line (`foxing notes.txt`) or by dragging a file
  onto the window
- Unsaved-changes prompt on close

## Install

Download from [Releases](https://github.com/1kevgriff/Foxing/releases):

- `foxing-<version>-x64.msi` installs for the current user (no admin
  prompt), adds a Start Menu shortcut, and adds Foxing to "Open with" for
  `.txt` files.
- `foxing.exe` runs as-is from anywhere.

The builds are unsigned, so Windows SmartScreen may warn on first run.

## Build

Requires Rust (version pinned in `rust-toolchain.toml`) and the MSVC
build tools.

```powershell
cargo build --release          # target/release/foxing.exe
./scripts/verify.ps1           # fmt, clippy, size and DLL gates, all tests
./scripts/build-msi.ps1        # MSI via WiX (needs the .NET SDK)
```

The end-to-end tests launch the real exe and drive it with window
messages, so run them on a desktop session.

## Release

Bump `version` in `Cargo.toml`, commit, then push a matching tag:

```powershell
git tag v0.1.0
git push origin v0.1.0
```

The release workflow verifies the build, tests the MSI, and publishes a
GitHub release with the exe, a zip, the MSI, and SHA-256 checksums.
