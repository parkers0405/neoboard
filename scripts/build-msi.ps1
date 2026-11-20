# Run after package-release.py on a clean, elevated Windows x64 runner.
$ErrorActionPreference = 'Stop'
$dist = (Resolve-Path "$PSScriptRoot/../dist").Path
$zip = Join-Path $dist 'neoboard-windows-x86_64.zip'
$name = 'NeoBoard-x86_64.msi'
$work = Join-Path ([IO.Path]::GetTempPath()) (New-Guid).Guid
New-Item -ItemType Directory $work | Out-Null
try {
    $payload = Join-Path $work 'payload'
    Expand-Archive -LiteralPath $zip -DestinationPath $payload
    $version = & "$payload/neoboard.exe" --version
    if ($LASTEXITCODE -ne 0 -or $version -notmatch '^\S+ (\d+\.\d+\.\d+)(?:[-+][\w.+-]+)?$') {
        throw "Cannot determine MSI version from bundled CLI: $version"
    }
    $msi = Join-Path $work $name
    $installerVersion = $Matches[1]
    # Isolate the pinned WiX CLI from other jobs and preinstalled WiX versions.
    dotnet tool install wix --version 5.0.2 --tool-path "$work/tools"
    if ($LASTEXITCODE -ne 0) { throw 'WiX tool installation failed' }
    $icon = (Resolve-Path "$PSScriptRoot/../design/NeoBoard.ico").Path
    & "$work/tools/wix.exe" build "$PSScriptRoot/neoboard.wxs" -arch x64 -d "Version=$installerVersion" -d "BinaryDir=$payload" -d "IconFile=$icon" -intermediateFolder "$work/obj" -o $msi
    if ($LASTEXITCODE -ne 0) { throw 'WiX MSI build failed' }
    & "$PSScriptRoot/verify-windows-msi.ps1" -Msi $msi -Zip $zip

    $hash = (Get-FileHash -LiteralPath $msi -Algorithm SHA256).Hash.ToLowerInvariant()
    Move-Item -LiteralPath $msi -Destination (Join-Path $dist $name) -Force
    [IO.File]::WriteAllText((Join-Path $dist "$name.sha256"), "$hash  $name`n", [Text.Encoding]::ASCII)
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force
}