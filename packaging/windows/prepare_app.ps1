param([Parameter(Mandatory = $true)][string]$AppExe, [string]$ManifestTool)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$AppPath = (Resolve-Path -LiteralPath $AppExe).Path
if ((Get-Item -LiteralPath $AppPath).Attributes -band [IO.FileAttributes]::ReparsePoint) {
    throw 'The app resource target must be a regular file.'
}
if ((Get-AuthenticodeSignature -LiteralPath $AppPath).Status -ne 'NotSigned') {
    throw 'Prepare Windows identity before signing the executable.'
}
if (-not $ManifestTool) {
    $Tool = Get-Command mt.exe -ErrorAction SilentlyContinue
    if ($Tool) { $ManifestTool = $Tool.Source }
    else {
        $SdkBin = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
        $Candidates = Get-ChildItem -LiteralPath $SdkBin -Directory |
            Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
            Sort-Object { [version]$_.Name } -Descending
        foreach ($Candidate in $Candidates) {
            $Path = Join-Path $Candidate.FullName 'x64\mt.exe'
            if (Test-Path -LiteralPath $Path -PathType Leaf) { $ManifestTool = $Path; break }
        }
    }
}
if (-not $ManifestTool) { throw 'Windows SDK mt.exe is required to prepare the app identity.' }
$ManifestTool = (Resolve-Path -LiteralPath $ManifestTool).Path
$Fragment = (Resolve-Path -LiteralPath "$PSScriptRoot/../../crates/convt-app/resources/convt-app.manifest").Path
# GPUI supplies RT_MANIFEST 1 (including DPI settings). Merge into that existing
# resource after linking; linking a second resource with ID 1 is invalid.
& $ManifestTool -nologo "-inputresource:$AppPath;#1" -manifest $Fragment "-outputresource:$AppPath;#1"
if ($LASTEXITCODE -ne 0) { throw 'Windows application manifest merge failed.' }
