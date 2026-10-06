# Optimize-step benchmark: launch and 5 MB open, baseline vs candidate, interleaved.
#   ./scripts/bench.ps1                      # latest release vs target/release/foxing.exe
#   ./scripts/bench.ps1 -Baseline v0.1.1     # a specific release tag
#   ./scripts/bench.ps1 -Baseline old.exe    # a local exe
# Medians are reported. CPU time is the stable signal; wall time is noisy on machines
# with slow process creation (AV/EDR). Timer granularity is 15.6 ms.
param(
    [string]$Baseline = 'latest',
    [string]$Candidate = 'target/release/foxing.exe',
    [int]$Runs = 7,
    [int]$Rounds = 2
)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
$work = New-Item -ItemType Directory -Force (Join-Path ([IO.Path]::GetTempPath()) 'foxing-bench')

function Resolve-Exe([string]$spec, [string]$name) {
    if (Test-Path $spec) {
        $dest = Join-Path $work "$name.exe"
        Copy-Item $spec $dest -Force
        return $dest
    }
    # Otherwise a release tag (or "latest"): download its exe.
    $dir = New-Item -ItemType Directory -Force (Join-Path $work "release-$spec")
    $tag = if ($spec -eq 'latest') { @() } else { @($spec) }
    gh release download @tag --repo 1kevgriff/Foxing --pattern foxing.exe --dir $dir --clobber
    if ($LASTEXITCODE -ne 0) { throw "could not download foxing.exe for '$spec'" }
    $dest = Join-Path $work "$name.exe"
    Copy-Item (Join-Path $dir 'foxing.exe') $dest -Force
    $dest
}

# Copies get distinct names so a stray process from one can't be mistaken for the other.
$exes = [ordered]@{
    baseline  = Resolve-Exe $Baseline 'baseline'
    candidate = Resolve-Exe $Candidate 'candidate'
}

$big = Join-Path $work 'big.txt'
if (-not (Test-Path $big)) {
    $line = "0123456789abcdefghijklmnopqrstuvwxyz café ✓ 0123456789abcdefghijklmnopqrstuvwxyz`n"
    [IO.File]::WriteAllText($big, $line * [int](5MB / $line.Length))
}

# Completion = window title shows the document, which is set right after the text loads.
# WaitForInputIdle is not used: it gave false timeouts for every build tested.
function Bench([string]$exe, [string]$file) {
    $want = if ($file) { "$(Split-Path $file -Leaf) - Foxing" } else { 'Untitled - Foxing' }
    $cpu = @(); $wall = @()
    for ($i = 0; $i -lt $Runs; $i++) {
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $p = if ($file) { Start-Process $exe -ArgumentList "`"$file`"" -PassThru } else { Start-Process $exe -PassThru }
        try {
            while ($p.MainWindowTitle -ne $want) {
                if ($sw.ElapsedMilliseconds -gt 30000) { throw "timed out waiting for '$want'" }
                Start-Sleep -Milliseconds 2
                $p.Refresh()
            }
            $wall += $sw.Elapsed.TotalMilliseconds
            $cpu += $p.TotalProcessorTime.TotalMilliseconds
        } finally {
            $p.Kill(); $p.WaitForExit()
        }
    }
    $median = { param($a) ($a | Sort-Object)[[int][math]::Floor($a.Count / 2)] }
    [pscustomobject]@{ Cpu = & $median $cpu; Wall = & $median $wall }
}

$rows = foreach ($round in 1..$Rounds) {
    foreach ($name in $exes.Keys) {
        foreach ($case in 'launch', 'open 5 MB') {
            $r = Bench $exes[$name] $(if ($case -eq 'launch') { $null } else { $big })
            [pscustomobject]@{ Round = $round; Build = $name; Case = $case; CpuMs = $r.Cpu; WallMs = $r.Wall }
        }
    }
}

"Baseline:  $Baseline ($((Get-Item $exes.baseline).Length) bytes)"
"Candidate: $Candidate ($((Get-Item $exes.candidate).Length) bytes)"
''
$rows | Group-Object Case, Build | ForEach-Object {
    [pscustomobject]@{
        Case   = $_.Group[0].Case
        Build  = $_.Group[0].Build
        CpuMs  = ($_.Group.CpuMs | ForEach-Object { '{0:n1}' -f $_ }) -join ' / '
        WallMs = ($_.Group.WallMs | ForEach-Object { '{0:n0}' -f $_ }) -join ' / '
    }
} | Format-Table -AutoSize
"(values per round, '/'-separated)"
