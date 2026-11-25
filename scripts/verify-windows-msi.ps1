# Installs and removes the MSI. Use a disposable, elevated x64 Windows runner.
# For the separate interactive/token check see docs/windows-msi-smoke.md.
param(
    [string]$Msi = "$PSScriptRoot/../dist/NeoBoard-x86_64.msi",
    [Parameter(Mandatory)][string]$Payload
)
$ErrorActionPreference = 'Stop'
$Msi = (Resolve-Path -LiteralPath $Msi).Path
$install = Join-Path $env:ProgramW6432 'NeoBoard'
if (Test-Path -LiteralPath $install) { throw "Refusing to test over an existing $install" }
$work = Join-Path ([IO.Path]::GetTempPath()) "neoboard-msi-$(New-Guid)"
New-Item -ItemType Directory $work | Out-Null
Write-Host "MSI verification logs (retained on failure): $work"

function Invoke-Installer($action, $log) {
    $process = Start-Process msiexec.exe -ArgumentList "/$action `"$Msi`" /qn /norestart /L*v `"$work/$log.log`"" -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "msiexec /$action failed ($($process.ExitCode)); see $work/$log.log" }
}

function Assert-NoDesktop($stage) {
    if (Get-Process -Name 'neochat-desktop' -ErrorAction SilentlyContinue) {
        throw "Unexpected running neochat-desktop.exe ($stage); see $work"
    }
}

# Inspect the linked MSI: hosted CI has no interactive user to click Finish.
function Assert-MsiColumn($query, [string[]]$expected) {
    $view = $db.OpenView($query)
    try {
        $view.Execute()
        $actual = @(while ($null -ne ($row = $view.Fetch())) {
            try { $row.StringData(1) } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($row) }
        })
        if ($actual.Count -ne $expected.Count -or ($actual -join "`n") -cne ($expected -join "`n")) {
            throw "MSI query: $query -- expected [$expected], got [$actual]"
        }
    } finally {
        $view.Close()
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($view)
    }
}

$installer = New-Object -ComObject WindowsInstaller.Installer
$db = $installer.OpenDatabase($Msi, 0) # read-only
try {
    Assert-MsiColumn 'SELECT `Value` FROM `Property` WHERE `Property` = ''WIXUI_EXITDIALOGOPTIONALCHECKBOX''' '1'
    Assert-MsiColumn 'SELECT `Property` FROM `Control` WHERE `Dialog_` = ''ExitDialog'' AND `Type` = ''CheckBox''' 'WIXUI_EXITDIALOGOPTIONALCHECKBOX'
    Assert-MsiColumn 'SELECT `Value` FROM `Property` WHERE `Property` = ''WIXUI_EXITDIALOGOPTIONALCHECKBOXTEXT''' 'Launch NeoBoard'
    Assert-MsiColumn 'SELECT `Value` FROM `Property` WHERE `Property` = ''WixUnelevatedShellExecTarget''' '[#NeoBoardDesktopExe]'
    # The File table may store an 8.3 alias before the long filename. Check the actual GUI key,
    # not just a property which could point at a missing file or at the console launcher.
    $view = $db.OpenView('SELECT `FileName` FROM `File` WHERE `File` = ''NeoBoardDesktopExe''')
    try {
        $view.Execute()
        $file = $view.Fetch()
        if (!$file -or ($file.StringData(1) -split '\|')[-1] -cne 'neochat-desktop.exe') { throw 'Invalid desktop launch target' }
    } finally {
        if ($file) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($file) }
        $view.Close()
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($view)
    }
    Assert-MsiColumn 'SELECT `Source` FROM `CustomAction` WHERE `Action` = ''LaunchNeoBoardDesktop''' 'Wix4UtilCA_X64'
    Assert-MsiColumn 'SELECT `Type` FROM `CustomAction` WHERE `Action` = ''LaunchNeoBoardDesktop''' '65' # DLL (1) + continue on return (64); immediate, no NoImpersonate
    Assert-MsiColumn 'SELECT `Target` FROM `CustomAction` WHERE `Action` = ''LaunchNeoBoardDesktop''' 'WixUnelevatedShellExec'
    Assert-MsiColumn 'SELECT `Sequence` FROM `InstallUISequence` WHERE `Action` = ''ExitDialog''' '-1' # on success
    Assert-MsiColumn 'SELECT `Condition` FROM `ControlEvent` WHERE `Dialog_` = ''ExitDialog'' AND `Control_` = ''Finish'' AND `Event` = ''DoAction'' AND `Argument` = ''LaunchNeoBoardDesktop''' 'WIXUI_EXITDIALOGOPTIONALCHECKBOX = 1 AND UILevel = 5 AND ACTION = "INSTALL" AND NOT Installed AND NOT REMOVE AND NOT REINSTALL'
    Assert-MsiColumn 'SELECT `Event` FROM `ControlEvent` WHERE `Dialog_` = ''ExitDialog'' AND `Control_` = ''Finish'' ORDER BY `Ordering`' @('DoAction', 'EndDialog')
    Assert-MsiColumn 'SELECT `Control_` FROM `ControlEvent` WHERE `Argument` = ''LaunchNeoBoardDesktop''' 'Finish'
    Assert-MsiColumn 'SELECT `Action` FROM `InstallExecuteSequence` WHERE `Action` = ''LaunchNeoBoardDesktop''' @()
    Assert-MsiColumn 'SELECT `Action` FROM `InstallUISequence` WHERE `Action` = ''LaunchNeoBoardDesktop''' @()
} finally {
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($db)
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer)
}

Assert-NoDesktop 'before install; use a clean runner'
Invoke-Installer i install
try {
    Assert-NoDesktop 'after quiet install'
    $expected = @(Get-ChildItem -LiteralPath $Payload -Force)
    $actual = @(Get-ChildItem -LiteralPath $install -Force)
    if (Compare-Object $expected.Name $actual.Name) { throw 'Installed file list differs from the MSI payload' }
    foreach ($file in $expected) {
        $installedFile = Join-Path $install $file.Name
        if ((Get-FileHash -LiteralPath $installedFile).Hash -ne (Get-FileHash -LiteralPath $file.FullName).Hash) {
            throw "Installed file differs from MSI payload: $($file.Name)"
        }
    }
    & "$install/neoboard.exe" --version
    if ($LASTEXITCODE -ne 0) { throw 'Installed CLI smoke test failed' }
    python "$PSScriptRoot/smoke-release.py" installed $install
    if ($LASTEXITCODE -ne 0) { throw 'Installed Windows release smoke test failed' }
    Invoke-Installer fa repair
    Assert-NoDesktop 'after quiet repair'
} finally {
    Invoke-Installer x uninstall
    Assert-NoDesktop 'after quiet uninstall'
    if (Test-Path -LiteralPath $install) { throw "Uninstall left $install behind" }
}
foreach ($log in 'install', 'repair', 'uninstall') {
    if (Select-String -LiteralPath "$work/$log.log" -Pattern 'Action start .*: LaunchNeoBoardDesktop\.') {
        throw "Quiet $log invoked the GUI launcher; see $work/$log.log"
    }
}
Remove-Item -LiteralPath $work -Recurse -Force
Write-Host 'PASS: linked MSI Finish wiring, quiet install/repair/uninstall, payload and CLI.'