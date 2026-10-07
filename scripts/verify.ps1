# Full verification pipeline. Fails fast; prints a summary table.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

# 300 -> 400 KB: custom text engine (#14). 400 -> 512 KB: custom UI (#19), settings (#24),
# folder sidebar + picker (#10), with room for UI Automation (#25).
$MaxBytes = 512KB
$AllowedDlls = 'kernel32', 'user32', 'gdi32', 'comdlg32', 'shell32', 'comctl32', 'imm32', 'dwmapi', 'advapi32', 'ole32', 'combase', 'ntdll', 'api-ms-win-core-*'
$results = [System.Collections.Generic.List[object]]::new()

function Step($name, [scriptblock]$body) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    try {
        $detail = & $body
        $results.Add([pscustomobject]@{ Step = $name; Result = 'PASS'; Time = "{0:n1}s" -f $sw.Elapsed.TotalSeconds; Detail = "$detail" })
    } catch {
        $results.Add([pscustomobject]@{ Step = $name; Result = 'FAIL'; Time = "{0:n1}s" -f $sw.Elapsed.TotalSeconds; Detail = "$_" })
        $results | Format-Table -AutoSize
        exit 1
    }
}

function Run([string]$exe, [string[]]$argv) {
    # Stream output to the console (not into the step result) so failures are visible.
    & $exe @argv | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "$exe $argv exited $LASTEXITCODE" }
}

Step 'fmt' { Run cargo @('fmt', '--check') }
Step 'clippy' { Run cargo @('clippy', '--release', '--all-targets', '--', '-D', 'warnings') }
Step 'build' { Run cargo @('build', '--release') }

$exePath = 'target/release/foxing.exe'
Step 'size' {
    $len = (Get-Item $exePath).Length
    if ($len -gt $MaxBytes) { throw "$len bytes > $MaxBytes" }
    "{0:n0} KB" -f ($len / 1KB)
}

Step 'dlls' {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $dumpbin = if (Test-Path $vswhere) {
        & $vswhere -latest -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' | Select-Object -First 1
    }
    if (-not $dumpbin) { return 'skipped (dumpbin not found)' }
    $dlls = & $dumpbin /dependents $exePath | Where-Object { $_ -match '^\s+(\S+)\.dll$' } | ForEach-Object { $Matches[1].ToLower() }
    $bad = $dlls | Where-Object { $d = $_; -not ($AllowedDlls | Where-Object { $d -like $_ }) }
    if ($bad) { throw "non-system DLLs: $($bad -join ', ')" }
    $dlls -join ' '
}

Step 'test' { Run cargo @('test', '--release', '--', '--test-threads=1') }

$results | Format-Table -AutoSize
