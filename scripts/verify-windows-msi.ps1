# Installs and removes the MSI. Use a disposable, elevated x64 Windows runner.
param(
    [string]$Msi = "$PSScriptRoot/../dist/NeoBoard-x86_64.msi",
    [string]$Zip = "$PSScriptRoot/../dist/neoboard-windows-x86_64.zip"
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

$reference = Join-Path $work 'reference'
Expand-Archive -LiteralPath $Zip -DestinationPath $reference
Invoke-Installer i install
try {
    $expected = @(Get-ChildItem -LiteralPath $reference -Force)
    $actual = @(Get-ChildItem -LiteralPath $install -Force)
    if (Compare-Object $expected.Name $actual.Name) { throw 'Installed file list differs from the release ZIP' }
    foreach ($file in $expected) {
        $installedFile = Join-Path $install $file.Name
        if ((Get-FileHash -LiteralPath $installedFile).Hash -ne (Get-FileHash -LiteralPath $file.FullName).Hash) {
            throw "Installed file differs from ZIP: $($file.Name)"
        }
    }
    & "$install/neoboard.exe" --version
    if ($LASTEXITCODE -ne 0) { throw 'Installed CLI smoke test failed' }
} finally {
    Invoke-Installer x uninstall
    if (Test-Path -LiteralPath $install) { throw "Uninstall left $install behind" }
}
Remove-Item -LiteralPath $work -Recurse -Force