# Interactive UI smoke test: real keystrokes, real file dialogs, screenshots.
# Takes over keyboard focus for ~20 s. Screenshots land in target/ui-smoke/.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class W {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public struct RECT { public int L, T, R, B; }
}
'@
[W]::SetProcessDPIAware() | Out-Null

$exe = (Resolve-Path 'target/release/foxing.exe').Path
$out = New-Item -ItemType Directory -Force 'target/ui-smoke'
$work = New-Item -ItemType Directory -Force (Join-Path $env:TEMP "foxing-ui-$PID")
$shell = New-Object -ComObject WScript.Shell
$results = [System.Collections.Generic.List[object]]::new()

function Check($name, [bool]$ok, $detail = '') {
    $results.Add([pscustomobject]@{ Check = $name; Result = $(if ($ok) { 'PASS' } else { 'FAIL' }); Detail = $detail })
}
function ForegroundTitle {
    $sb = New-Object System.Text.StringBuilder 256
    [W]::GetWindowText([W]::GetForegroundWindow(), $sb, 256) | Out-Null
    $sb.ToString()
}
function WaitTitle($pattern, $ms = 5000) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $ms) {
        if ((ForegroundTitle) -like $pattern) { return $true }
        Start-Sleep -Milliseconds 50
    }
    $false
}
# Refuse to send keys unless a Foxing window (or its dialog) has focus.
function Keys($k) {
    $fpid = 0; [W]::GetWindowThreadProcessId([W]::GetForegroundWindow(), [ref]$fpid) | Out-Null
    if ((Get-Process -Id $fpid -ErrorAction SilentlyContinue).Name -ne 'foxing') {
        throw "Foreground is '$(ForegroundTitle)', not Foxing; aborting before sending keys."
    }
    [System.Windows.Forms.SendKeys]::SendWait($k); Start-Sleep -Milliseconds 150
}
function Shot($name) {
    Start-Sleep -Milliseconds 250
    $h = [W]::GetForegroundWindow(); $r = New-Object W+RECT
    [W]::GetWindowRect($h, [ref]$r) | Out-Null
    $bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
    $bmp.Save((Join-Path $out "$name.png")); $g.Dispose(); $bmp.Dispose()
}
function Launch($arg) {
    $p = if ($arg) { Start-Process $exe -ArgumentList "`"$arg`"" -PassThru } else { Start-Process $exe -PassThru }
    $p.WaitForInputIdle(10000) | Out-Null
    while ($p.MainWindowHandle -eq 0) { Start-Sleep -Milliseconds 20; $p.Refresh() }
    $shell.AppActivate($p.Id) | Out-Null
    [W]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
    Start-Sleep -Milliseconds 300
    $p
}

# 1. Open via CLI, render, accelerators for find.
$doc = Join-Path $work 'doc.txt'
$long = 'This is a deliberately long line that should wrap when Word Wrap is on and scroll horizontally when it is off. ' * 2
Set-Content $doc -Encoding utf8 -Value "alpha beta gamma`r`nsecond line with beta`r`n$long`r`nunicode: café ✓ 日本語"
$p = Launch $doc
Check 'cli open title' (WaitTitle 'doc.txt - Foxing') (ForegroundTitle)
Shot '1-opened'

Keys '^f'
Check 'Ctrl+F opens Find' (WaitTitle 'Find') (ForegroundTitle)
Keys 'beta'; Keys '{ENTER}'; Shot '2-find-first'; Keys '{ESC}'
Check 'Esc closes Find' (WaitTitle 'doc.txt - Foxing') (ForegroundTitle)
Keys '{F3}'; Shot '3-f3-next'

Keys '%o'; Keys 'w'; Shot '4-wrap-on'
Keys '%o'; Keys 'w'; Shot '5-wrap-off'

# 2. Typing marks dirty; Ctrl+S saves in place.
Keys '^{END}'; Keys '{ENTER}added by keystrokes'
Check 'typing marks dirty' (WaitTitle '`*doc.txt - Foxing') (ForegroundTitle)
Keys '^s'
Check 'Ctrl+S clears dirty' (WaitTitle 'doc.txt - Foxing') (ForegroundTitle)
Check 'Ctrl+S wrote file' ((Get-Content $doc -Raw) -match 'added by keystrokes')
$p | Stop-Process -Force

# 3. Untitled -> Save As dialog.
$saved = Join-Path $work 'saved-as.txt'
$p = Launch $null
Keys 'saved through dialog'
Keys '^s'
Check 'Save As dialog opens' (WaitTitle 'Save As') (ForegroundTitle)
Shot '6-save-as-dialog'
Keys '^a'; Keys ($saved -replace '([+^%~(){}\[\]])', '{$1}'); Keys '{ENTER}'
Check 'Save As title' (WaitTitle 'saved-as.txt - Foxing') (ForegroundTitle)
Check 'Save As wrote file' ((Test-Path $saved) -and (Get-Content $saved -Raw) -eq 'saved through dialog')

# 4. Ctrl+O -> Open dialog.
Keys '^o'
Check 'Open dialog opens' (WaitTitle 'Open') (ForegroundTitle)
Keys ($doc -replace '([+^%~(){}\[\]])', '{$1}'); Keys '{ENTER}'
Check 'Open loads file' (WaitTitle 'doc.txt - Foxing') (ForegroundTitle)
Shot '7-opened-via-dialog'

# 5. Dirty close prompt via Alt+F4.
Keys 'x'; Keys '%{F4}'
Check 'Alt+F4 prompts when dirty' (WaitTitle 'Foxing') (ForegroundTitle)
Shot '8-save-prompt'
Keys 'n'
Start-Sleep -Milliseconds 500
Check 'Don''t Save exits' $p.HasExited
$p | Stop-Process -Force -ErrorAction SilentlyContinue

Remove-Item -Recurse -Force $work
$results | Format-Table -AutoSize
"Screenshots: $out"
if ($results.Result -contains 'FAIL') { exit 1 }
