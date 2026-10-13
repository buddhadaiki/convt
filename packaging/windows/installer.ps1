param([string]$WixDll, [string]$InstallerVersion, [string]$WindowsSdkInclude)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Resolve-MsiInstallerVersion([string]$InstallerVersion, [string]$AppVersion) {
    $Value = if ($InstallerVersion) { $InstallerVersion } else { $AppVersion }
    $Message = "Invalid MSI installer version '$Value': use major.minor.build with maximum 255.255.65535."
    if ($Value -notmatch '\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z') { throw $Message }
    $Limits = @(255, 255, 65535)
    for ($Index = 0; $Index -lt 3; $Index++) {
        [uint32]$Number = 0
        if (-not [uint32]::TryParse($matches[$Index + 1], [ref]$Number) -or $Number -gt $Limits[$Index]) {
            throw $Message
        }
    }
    $Value
}

$Repo = (Resolve-Path "$PSScriptRoot/../..").Path
Set-Location $Repo
$AppVersion = ((Get-Content Cargo.toml | Select-String '^version = "([0-9.]+)"$' | Select-Object -First 1).Matches[0].Groups[1].Value)
$Version = Resolve-MsiInstallerVersion -InstallerVersion $InstallerVersion -AppVersion $AppVersion
$Payload = Join-Path $Repo 'packaging/out/windows/payload'
if (!(Test-Path "$Payload/build-receipt.json") -or !(Test-Path "$Payload/convt-app.exe")) { throw 'Build verified payload first' }
& "$PSScriptRoot/prepare_app.ps1" -AppExe "$Payload/convt-app.exe"
python "$PSScriptRoot/prepare_identity.py" $Payload --version $Version
if ($LASTEXITCODE -ne 0) { throw 'Windows identity staging failed' }
$WixVersion = '6.0.2'
$Tools = Join-Path $Repo 'packaging/cache/windows/wix'
if ($WixDll) {
    $WixDll = (Resolve-Path -LiteralPath $WixDll).Path
    $WixCommand = (Get-Command dotnet.exe -ErrorAction Stop).Source
    # A restored WiX package can run on the .NET runtime without an SDK.
    $WixArgs = @('--roll-forward', 'Major', $WixDll)
} else {
    if (!(Test-Path "$Tools/wix.exe")) {
        dotnet tool install --tool-path $Tools wix --version $WixVersion --add-source https://api.nuget.org/v3/index.json
        if ($LASTEXITCODE -ne 0) { throw 'WiX installation failed' }
    }
    $WixCommand = "$Tools/wix.exe"
    $WixArgs = @()
}
$CachedVersion = (& $WixCommand @WixArgs --version | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or ($CachedVersion -split '\+')[0] -ne $WixVersion) {
    throw "Cached WiX version '$CachedVersion' does not match pin $WixVersion. Remove $Tools and rerun."
}
# Derived harvest: replace only this owned directory so a previous WiX
# failure can retry without rebuilding the verified payload.
$Stage = Join-Path $Repo 'packaging/out/windows/msi-payload'
$OwnedRoot = [System.IO.Path]::GetFullPath((Join-Path $Repo 'packaging/out/windows'))
$ExpectedStage = [System.IO.Path]::GetFullPath($Stage)
if ([System.IO.Path]::GetFileName($ExpectedStage) -ne 'msi-payload') {
    throw "Refusing to remove unexpected harvest path: $ExpectedStage"
}
if ([System.IO.Path]::GetDirectoryName($ExpectedStage) -ne $OwnedRoot) {
    throw "Refusing to remove ${ExpectedStage}: outside packaging/out/windows"
}
$Existing = Get-Item -LiteralPath $ExpectedStage -ErrorAction SilentlyContinue
if ($Existing) {
    if (-not $Existing.PSIsContainer) { throw "Refusing to remove $($Existing.FullName): not a directory" }
    if ($Existing.Name -ne 'msi-payload') { throw "Refusing to remove $($Existing.FullName): not the MSI harvest directory" }
    if ($Existing.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Refusing to remove reparse point $($Existing.FullName)"
    }
    $Parent = Get-Item -LiteralPath $Existing.Parent.FullName
    if ($Parent.FullName -ne $OwnedRoot) { throw "Refusing to remove $($Existing.FullName): outside packaging/out/windows" }
    if ($Parent.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Refusing to remove $($Existing.FullName) through a reparse parent"
    }
    Remove-Item -LiteralPath $Existing.FullName -Recurse -Force
}
python "$PSScriptRoot/stage_payload.py" $Payload $Stage
if ($LASTEXITCODE -ne 0) { throw 'Windows MSI payload staging failed' }
$ActionOutput = Join-Path $Repo 'packaging/out/windows/installer-actions'
$ActionBuild = @{ OutputDirectory=$ActionOutput }
if ($WindowsSdkInclude) { $ActionBuild.WindowsSdkInclude=$WindowsSdkInclude }
& "$PSScriptRoot/build-installer-actions.ps1" @ActionBuild
$ActionsDll = Join-Path $ActionOutput 'convt-installer-actions.dll'
if (-not (Test-Path -LiteralPath $ActionsDll -PathType Leaf)) { throw 'Embedded installer actions were not built' }
$Msi = Join-Path $Repo "packaging/out/windows/convt-$Version-windows-x86_64.msi"
& $WixCommand @WixArgs build -arch x64 -d "Version=$Version" -d "Payload=$Stage" -d "InstallerActionsDll=$ActionsDll" "$PSScriptRoot/convt.wxs" -o $Msi
if ($LASTEXITCODE -ne 0) { throw 'WiX installer build failed' }
$Built = Get-Item $Msi
$Limit = 120MB
if ($Built.Length -ge $Limit) {
    throw ("{0} is {1:N0} bytes ({2:N1} MiB); the runtime MSI must stay under 120 MiB." -f $Built.Name, $Built.Length, ($Built.Length / 1MB))
}
Write-Host ("{0}: {1:N0} bytes ({2:N1} MiB)" -f $Built.Name, $Built.Length, ($Built.Length / 1MB))
