param(
    [Parameter(Mandatory = $true)]
    [Alias('Out', 'OutDirectory')]
    [string]$OutputDirectory,
    [string]$WindowsSdkInclude
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Import an existing toolchain only into this process. The helper never installs
# a toolchain, mutates the installer payload, or invokes a custom action.
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    $VSWhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    if (-not (Test-Path -LiteralPath $VSWhere -PathType Leaf)) { throw 'Existing Visual Studio C++ build tools are required.' }
    $VisualStudio = & $VSWhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -format json |
        ConvertFrom-Json | Select-Object -First 1
    if (-not $VisualStudio) { throw 'Existing Visual Studio C++ build tools are required.' }
    $VcVars = Join-Path $VisualStudio.installationPath 'VC/Auxiliary/Build/vcvars64.bat'
    if (-not (Test-Path -LiteralPath $VcVars -PathType Leaf) -or $VcVars -match '["\r\n]') {
        throw 'Existing Visual Studio vcvars64.bat is required.'
    }
    $VcEnvironment = & $env:ComSpec /d /s /c ('"{0}" >nul && set' -f $VcVars)
    if ($LASTEXITCODE -ne 0) { throw 'Importing the existing vcvars64 environment failed.' }
    foreach ($Entry in $VcEnvironment) {
        if ($Entry -match '^(PATH|INCLUDE|LIB|LIBPATH)=(.*)$') {
            [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
        }
    }
}
if ($WindowsSdkInclude) {
    $SdkRoot = (Resolve-Path -LiteralPath $WindowsSdkInclude).Path
    if ($SdkRoot -match '[;"\r\n]') { throw 'WindowsSdkInclude contains unsupported characters.' }
    $SdkDirectories = @('shared', 'um', 'ucrt', 'winrt', 'cppwinrt') | ForEach-Object {
        $Directory = Join-Path $SdkRoot $_
        if (-not (Test-Path -LiteralPath $Directory -PathType Container)) { throw "Windows SDK include directory is missing: $Directory" }
        $Directory
    }
    $env:INCLUDE = ($SdkDirectories -join ';') + ';' + $env:INCLUDE
}
$Compiler = (Get-Command cl.exe -ErrorAction Stop).Source
$ResourceCompiler = (Get-Command rc.exe -ErrorAction Stop).Source
if ($Compiler -notmatch '[\\/]Hostx64[\\/]x64[\\/]cl\.exe$') {
    throw "The installer actions require an existing x64 MSVC environment: $Compiler"
}
if (-not $env:INCLUDE -or -not $env:LIB) {
    throw 'The installer actions require an existing MSVC INCLUDE and LIB environment.'
}
foreach ($Header in @('windows.h', 'msi.h', 'msiquery.h', 'shlobj.h', 'objbase.h', 'userenv.h', 'winapifamily.h', 'stdlib.h')) {
    $Found = $false
    foreach ($IncludeDirectory in ($env:INCLUDE -split ';')) {
        if ($IncludeDirectory -and (Test-Path -LiteralPath (Join-Path $IncludeDirectory $Header) -PathType Leaf)) {
            $Found = $true
            break
        }
    }
    if (-not $Found) { throw "MSVC/Windows SDK header is missing: $Header" }
}

$Source = Join-Path $PSScriptRoot 'installer-actions.cpp'
$ResourceSource = Join-Path $PSScriptRoot 'installer-actions.rc'
$ModernSource = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot 'modern-menu.ps1')).Path
$FinishSource = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot 'finish-options.ps1')).Path
foreach ($Path in @($ModernSource, $FinishSource, $OutputDirectory)) {
    if ($Path -match '["\r\n]') { throw 'Build paths contain unsupported characters.' }
}
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$BuildDirectory = Join-Path $OutputDirectory ('installer-actions-build-' + [guid]::NewGuid().ToString('D'))
New-Item -ItemType Directory -Path $BuildDirectory | Out-Null
$Resource = Join-Path $BuildDirectory 'installer-actions.res'
$Object = Join-Path $BuildDirectory 'installer-actions.obj'
$Dll = Join-Path $OutputDirectory 'convt-installer-actions.dll'
$ImportLibrary = Join-Path $BuildDirectory 'convt-installer-actions.lib'

# Fixed resource names are resolved from the source directory. This also avoids
# PowerShell 5.1 stripping embedded quotes from string-macro command arguments.
Push-Location -LiteralPath $PSScriptRoot
try {
    & $ResourceCompiler /nologo "/fo$Resource" $ResourceSource
    if ($LASTEXITCODE -ne 0) { throw 'Compiling installer helper resources failed.' }
} finally {
    Pop-Location
}

$Arguments = @('/nologo', '/LD', '/MT', '/O2', '/std:c++17', '/EHsc', '/W4', '/utf-8',
    '/DUNICODE', '/D_UNICODE', "/Fo$Object", $Source, $Resource, '/link',
    '/MACHINE:X64', "/OUT:$Dll", "/IMPLIB:$ImportLibrary", 'msi.lib', 'shell32.lib', 'ole32.lib',
    'advapi32.lib', 'userenv.lib')
& $Compiler @Arguments
if ($LASTEXITCODE -ne 0) { throw 'Compiling native installer actions failed.' }
if (-not (Test-Path -LiteralPath $Dll -PathType Leaf)) { throw 'The native installer actions DLL was not produced.' }
Write-Output $Dll
