# Silent per-user install -> verify -> uninstall -> verify removal. Leaves the machine as it was.
param([Parameter(Mandatory)][string]$Msi)
$ErrorActionPreference = 'Stop'
$Msi = (Resolve-Path $Msi).Path
$exe = Join-Path $env:LOCALAPPDATA 'Programs\Foxing\foxing.exe'
$lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Foxing.lnk'
$openWith = 'HKCU:\Software\Classes\Applications\foxing.exe\shell\open\command'
$log = Join-Path ([IO.Path]::GetTempPath()) 'foxing-msi.log'
$failures = @()

function Msiexec([string[]]$argv) {
    $p = Start-Process msiexec.exe -ArgumentList ($argv + @('/qn', '/l*v', "`"$log`"")) -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "msiexec $argv exited $($p.ExitCode); see $log" }
}
function Check($name, [bool]$ok) {
    "{0}  {1}" -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $name
    if (-not $ok) { $script:failures += $name }
}

# Per-user MSI products register here (this is what Apps & features reads).
function Registered {
    [bool](Get-ChildItem 'HKCU:\Software\Microsoft\Installer\Products' -ErrorAction SilentlyContinue |
        Where-Object { (Get-ItemProperty $_.PSPath).ProductName -eq 'Foxing' })
}

if (Test-Path $exe) { throw "Foxing already installed at $exe; uninstall it first." }

Msiexec @('/i', "`"$Msi`"")
try {
    Check 'exe installed' (Test-Path $exe)
    Check 'Start Menu shortcut' (Test-Path $lnk)
    Check 'Open With registration' ((Get-ItemProperty $openWith).'(default)' -like "*foxing.exe*%1*")
    Check 'registered with Windows Installer' (Registered)
    $p = Start-Process $exe -PassThru
    $p.WaitForInputIdle(10000) | Out-Null
    Check 'installed exe launches' (-not $p.HasExited)
    $p | Stop-Process -Force
    $p.WaitForExit()
} finally {
    Msiexec @('/x', "`"$Msi`"")
}
Check 'product unregistered' (-not (Registered))
Check 'exe removed' (-not (Test-Path $exe))
Check 'shortcut removed' (-not (Test-Path $lnk))
Check 'Open With removed' (-not (Test-Path $openWith))
if ($failures) { throw "MSI checks failed: $($failures -join ', ')" }
