# Renders Foxing with sample text to docs/screenshot.png via PrintWindow (no focus or input needed).
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class Snap {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
  public struct RECT { public int L, T, R, B; }
}
'@
[Snap]::SetProcessDPIAware() | Out-Null

$sample = Join-Path ([IO.Path]::GetTempPath()) 'notes.txt'
@'
Foxing — plain-text notes

Groceries
  - coffee beans
  - sourdough
  - lemons

Ideas
  - fast launch, tiny exe, no bells and whistles
  - Ctrl+F to find, F3 for next
  - Format > Word Wrap

Unicode works too: café, naïve, 日本語, ✓
'@ | Set-Content $sample -Encoding utf8

$p = Start-Process 'target/release/foxing.exe' -ArgumentList "`"$sample`"" -PassThru
try {
    $p.WaitForInputIdle(10000) | Out-Null
    while ($p.MainWindowHandle -eq 0) { Start-Sleep -Milliseconds 20; $p.Refresh() }
    $h = $p.MainWindowHandle
    [Snap]::SetWindowPos($h, [IntPtr]::Zero, 0, 0, 640, 360, 0x0016) | Out-Null  # NOMOVE|NOZORDER|NOACTIVATE
    Start-Sleep -Milliseconds 400

    $r = New-Object Snap+RECT
    [Snap]::GetWindowRect($h, [ref]$r) | Out-Null
    $bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    [Snap]::PrintWindow($h, $hdc, 2) | Out-Null  # PW_RENDERFULLCONTENT
    $g.ReleaseHdc($hdc)

    # Crop the invisible resize border (DWM extended frame) off the window rect.
    $f = New-Object Snap+RECT
    [Snap]::DwmGetWindowAttribute($h, 9, [ref]$f, 16) | Out-Null  # DWMWA_EXTENDED_FRAME_BOUNDS
    $crop = New-Object System.Drawing.Rectangle ($f.L - $r.L), ($f.T - $r.T), ($f.R - $f.L), ($f.B - $f.T)
    New-Item -ItemType Directory -Force docs | Out-Null
    $bmp.Clone($crop, $bmp.PixelFormat).Save((Join-Path (Get-Location) 'docs/screenshot.png'))
    $g.Dispose(); $bmp.Dispose()
} finally {
    $p | Stop-Process -Force
    Remove-Item $sample
}
'docs/screenshot.png'
