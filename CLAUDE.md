# Foxing

Minimal Win32 plain-text editor in Rust. Speed first: fast launch, fast open, no lag while typing.

## Feature workflow

Every feature ships in two steps, in this order:

1. **Build it.** Implement, add E2E coverage in `tests/e2e.rs`, `scripts/verify.ps1` green.
2. **Optimize it.** Before the feature is called done:
   - Measure launch (the `launch_time` E2E test prints CPU and wall time), large-file open,
     and exe size against the previous release.
   - Profile anything the feature put on a hot path: startup, file load, keystroke handling.
   - Remove or defer work that doesn't need to happen at startup (lazy-create controls,
     load on first use).
   - Record before/after numbers in the PR description.

Exe size may grow when a feature earns it. Raise the size gate in `scripts/verify.ps1`
deliberately, in the same PR, with the reason. Never raise it to make an unexplained
regression pass.

## Commands

```powershell
./scripts/verify.ps1          # fmt, clippy, build, size + DLL gates, unit + E2E tests
./scripts/build-msi.ps1       # MSI (WiX 5, pinned; do not upgrade to v6+)
./scripts/test-msi.ps1 -Msi <path>
./scripts/screenshot.ps1      # regenerate docs/screenshot.png
```

## Rules

- Never send synthetic keystrokes to the desktop without first confirming a Foxing window
  has focus (`scripts/ui-smoke.ps1` guards this).
- Dependencies: `windows-sys` only. Adding a crate needs a reason measured in the optimize step.
