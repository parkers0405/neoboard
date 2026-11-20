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
    # WixUI_Minimal displays an RTF license. Use the actual bundled license, not the WiX sample.
    $licenseRtf = Join-Path $work 'License.rtf'
    Add-Type -AssemblyName System.Windows.Forms
    $license = [System.Windows.Forms.RichTextBox]::new()
    try {
        $license.Text = [IO.File]::ReadAllText("$payload/LICENSE")
        $license.SaveFile($licenseRtf)
    } finally { $license.Dispose() }

    $wixVersion = '5.0.2'
    dotnet tool install wix --version $wixVersion --tool-path "$work/tools"
    if ($LASTEXITCODE -ne 0) { throw 'WiX tool installation failed' }
    $icon = (Resolve-Path "$PSScriptRoot/../design/NeoBoard.ico").Path
    $wix = "$work/tools/wix.exe"
    # Keep the matching project-local WiX extension cache in the disposable build directory.
    Push-Location $work
    try {
        foreach ($extension in 'WixToolset.UI.wixext', 'WixToolset.Util.wixext') {
            & $wix extension add "$extension/$wixVersion"
            if ($LASTEXITCODE -ne 0) { throw "WiX extension installation failed: $extension" }
        }
        & $wix build "$PSScriptRoot/neoboard.wxs" -arch x64 -ext WixToolset.UI.wixext -ext WixToolset.Util.wixext -d "Version=$installerVersion" -d "BinaryDir=$payload" -d "IconFile=$icon" -d "LicenseRtf=$licenseRtf" -intermediateFolder "$work/obj" -o $msi
        if ($LASTEXITCODE -ne 0) { throw 'WiX MSI build failed' }
    } finally { Pop-Location }
    & "$PSScriptRoot/verify-windows-msi.ps1" -Msi $msi -Zip $zip

    $hash = (Get-FileHash -LiteralPath $msi -Algorithm SHA256).Hash.ToLowerInvariant()
    Move-Item -LiteralPath $msi -Destination (Join-Path $dist $name) -Force
    [IO.File]::WriteAllText((Join-Path $dist "$name.sha256"), "$hash  $name`n", [Text.Encoding]::ASCII)
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force
}