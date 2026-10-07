# Foxing

Minimal Win32 plain-text editor in Rust. Speed first: fast launch, fast open, no lag while typing.

## Feature workflow

Every feature ships in two steps, in this order:

1. **Build it.** Implement, add E2E coverage in `tests/e2e.rs`, `scripts/verify.ps1` green.
2. **Optimize it.** Before the feature is called done:
   - Run `./scripts/bench.ps1` (latest release vs your build): launch and 5 MB open,
     CPU and wall time, plus exe size. CPU time is the signal; wall time is noisy here.
   - Profile anything the feature put on a hot path: startup, file load, keystroke handling.
   - Remove or defer work that doesn't need to happen at startup (lazy-create controls,
     load on first use).
   - Record before/after numbers in the PR description.

Exe size may grow when a feature earns it. Raise the size gate in `scripts/verify.ps1`
deliberately, in the same PR, with the reason. Never raise it to make an unexplained
regression pass.

## Portability

Mac and Linux versions are planned. Windows-only today, but structure code so the
platform shell stays thin:

- Logic goes in platform-neutral modules: `src/buffer.rs` (storage, line index, search),
  `src/editor.rs` (selection, movement, undo, wrap layout, scrolling), `src/document.rs`,
  `src/text.rs`. No Win32 types there; unit-test it there.
- Win32 code (`src/win.rs` shell, `src/textview.rs` view) only creates windows, paints
  rows the editor hands it, and turns input into editor calls.
- Text is stored as the file's bytes (line endings preserved); new line breaks use the
  file's detected EOL. Large-file targets: 2 GB opens, edits, searches, saves.
- Use `std::path` / `std::fs` for file work, not Win32 file APIs.
- Settings and paths: no hardcoded `%APPDATA%` outside the Win32 layer.

## Commands

```powershell
./scripts/verify.ps1          # fmt, clippy, build, size + DLL gates, unit + E2E tests
./scripts/build-msi.ps1       # MSI (WiX 5, pinned; do not upgrade to v6+)
./scripts/test-msi.ps1 -Msi <path>
./scripts/bench.ps1           # optimize step: latest release vs target/release build
./scripts/screenshot.ps1      # regenerate docs/screenshot.png
```

## Rules

- Never send synthetic keystrokes to the desktop without first confirming a Foxing window
  has focus (`scripts/ui-smoke.ps1` guards this).
- Dependencies: `windows-sys` only. Adding a crate needs a reason measured in the optimize step.
