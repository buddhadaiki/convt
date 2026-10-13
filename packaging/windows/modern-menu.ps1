# Register only convt's current-user package identity. Never enable Developer
# Mode, trust certificates, change the in-process CLSID or restart Explorer.
[CmdletBinding()]
param(
    [ValidateSet('Install', 'Remove', 'Status', 'Rollback', 'Commit', 'Probe')]
    [string]$Action = 'Status',
    [string]$InstallationRoot,
    [string]$SignedPackagePath,
    [switch]$RestoreClassic,
    [guid]$TransactionId = [guid]::Empty,
    [guid]$ClassId = [guid]::Empty
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:ConvtMenuSourcePath = $PSCommandPath

function Get-ConvtMenuIdentity {
    [pscustomobject]@{
        Name = 'Convt.Desktop'; Publisher = 'CN=Convt Local'; ApplicationId = 'ConvtApp'
        ClassId = '710FB9A8-C47E-4B39-9CFA-E273AB1B78F8'
        LegacyKey = 'Software\Classes\*\shell\convt'
    }
}

function Assert-ConvtIdentityVersion {
    param([string]$Version)
    if ($Version -notmatch '\A[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\z') { throw 'Identity version must contain four numeric components.' }
    $parsed = [version]$Version
    if ($parsed.ToString() -cne $Version -or @($parsed.Major, $parsed.Minor, $parsed.Build, $parsed.Revision | Where-Object { $_ -gt 65535 }).Count -gt 0) {
        throw 'Identity version must use canonical components in the range 0..65535.'
    }
}

function Assert-ConvtMenuManifest {
    param([xml]$Manifest)
    $spec = Get-ConvtMenuIdentity
    $ns = [System.Xml.XmlNamespaceManager]::new($Manifest.NameTable)
    $namespaces = @{
        p = 'http://schemas.microsoft.com/appx/manifest/foundation/windows10'
        uap = 'http://schemas.microsoft.com/appx/manifest/uap/windows10'
        uap10 = 'http://schemas.microsoft.com/appx/manifest/uap/windows10/10'
        com = 'http://schemas.microsoft.com/appx/manifest/com/windows10'
        desktop4 = 'http://schemas.microsoft.com/appx/manifest/desktop/windows10/4'
        desktop5 = 'http://schemas.microsoft.com/appx/manifest/desktop/windows10/5'
        rescap = 'http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities'
    }
    foreach ($entry in $namespaces.GetEnumerator()) { $ns.AddNamespace($entry.Key, $entry.Value) }
    $identity = $Manifest.SelectSingleNode('/p:Package/p:Identity', $ns)
    $app = $Manifest.SelectSingleNode('/p:Package/p:Applications/p:Application', $ns)
    $class = $Manifest.SelectSingleNode('//com:SurrogateServer/com:Class', $ns)
    $verb = $Manifest.SelectSingleNode('//desktop5:ItemType/desktop5:Verb', $ns)
    $visual = $Manifest.SelectSingleNode('//uap:VisualElements', $ns)
    $type = $Manifest.SelectSingleNode('//desktop5:ItemType', $ns)
    $external = $Manifest.SelectSingleNode('/p:Package/p:Properties/uap10:AllowExternalContent', $ns)
    $device = $Manifest.SelectSingleNode('/p:Package/p:Dependencies/p:TargetDeviceFamily', $ns)
    if (-not $identity -or -not $app -or -not $class -or -not $verb -or -not $visual -or -not $type -or -not $external -or -not $device) {
        throw 'convt identity manifest is missing required elements.'
    }
    if ($identity.GetAttribute('Name') -cne $spec.Name -or $identity.GetAttribute('Publisher') -cne $spec.Publisher -or
        $identity.GetAttribute('ProcessorArchitecture') -cne 'x64' -or
        $app.GetAttribute('Id') -cne $spec.ApplicationId -or $app.GetAttribute('Executable') -cne 'convt-app.exe' -or
        $app.GetAttribute('TrustLevel', $namespaces.uap10) -cne 'mediumIL' -or
        $app.GetAttribute('RuntimeBehavior', $namespaces.uap10) -cne 'win32App' -or
        $class.GetAttribute('Id') -cne $spec.ClassId -or $class.GetAttribute('Path') -cne 'convt_shell.dll' -or
        $class.GetAttribute('ThreadingModel') -cne 'STA' -or $verb.GetAttribute('Clsid') -cne $spec.ClassId -or
        $verb.GetAttribute('Id') -cne 'ConvertWithConvt' -or $type.GetAttribute('Type') -cne '*' -or
        $external.InnerText -cne 'true' -or $visual.GetAttribute('AppListEntry') -cne 'none' -or
        $device.GetAttribute('Name') -cne 'Windows.Desktop' -or $device.GetAttribute('MinVersion') -cne '10.0.22000.0') {
        throw 'convt identity manifest does not match the expected identity, files and Windows menu contract.'
    }
    Assert-ConvtIdentityVersion -Version $identity.GetAttribute('Version')
    $extensions = @($Manifest.SelectNodes('/p:Package/p:Applications/p:Application/p:Extensions/*', $ns))
    $categories = @($extensions | ForEach-Object { $_.GetAttribute('Category') } | Sort-Object)
    $capabilities = @($Manifest.SelectNodes('/p:Package/p:Capabilities/*', $ns))
    $capNames = @($capabilities | ForEach-Object { $_.GetAttribute('Name') } | Sort-Object)
    if ($Manifest.SelectNodes('/p:Package/p:Identity', $ns).Count -ne 1 -or
        $Manifest.SelectNodes('/p:Package/p:Applications/p:Application', $ns).Count -ne 1 -or
        $Manifest.SelectNodes('//com:SurrogateServer', $ns).Count -ne 1 -or
        $Manifest.SelectNodes('//com:Class', $ns).Count -ne 1 -or
        $Manifest.SelectNodes('//desktop5:ItemType', $ns).Count -ne 1 -or
        $Manifest.SelectNodes('//desktop5:Verb', $ns).Count -ne 1 -or
        $Manifest.SelectNodes('/p:Package/p:Extensions', $ns).Count -ne 0 -or
        $extensions.Count -ne 2 -or ($categories -join ',') -cne 'windows.comServer,windows.fileExplorerContextMenus' -or
        $capabilities.Count -ne 2 -or ($capNames -join ',') -cne 'runFullTrust,unvirtualizedResources' -or
        @($capabilities | Where-Object NamespaceURI -ne $namespaces.rescap).Count -ne 0 -or
        $class.ParentNode.HasAttribute('CustomSurrogateExecutable') -or $class.ParentNode.HasAttribute('LaunchAndActivationPermission')) {
        throw 'convt identity manifest contains unexpected registrations or capabilities.'
    }
    $surrogate = $class.ParentNode
    $server = $surrogate.ParentNode
    $comExtension = $server.ParentNode
    if ($server.LocalName -cne 'ComServer' -or $server.NamespaceURI -cne $namespaces.com -or
        $comExtension.LocalName -cne 'Extension' -or $comExtension.NamespaceURI -cne $namespaces.com -or
        @($server.ChildNodes | Where-Object NodeType -eq ([System.Xml.XmlNodeType]::Element)).Count -ne 1 -or
        @($surrogate.ChildNodes | Where-Object NodeType -eq ([System.Xml.XmlNodeType]::Element)).Count -ne 1 -or
        @($class.ChildNodes | Where-Object NodeType -eq ([System.Xml.XmlNodeType]::Element)).Count -ne 0 -or
        @($class.Attributes | Where-Object { $_.Name -notin @('Id', 'Path', 'ThreadingModel') }).Count -ne 0 -or
        @($surrogate.Attributes | Where-Object Name -ne 'DisplayName').Count -ne 0 -or
        @($server.Attributes).Count -ne 0) {
        throw 'convt identity manifest contains unexpected COM registrations or attributes.'
    }
    $assets = @(
        $Manifest.SelectSingleNode('/p:Package/p:Properties/p:Logo', $ns).InnerText
        $visual.GetAttribute('Square150x150Logo')
        $visual.GetAttribute('Square44x44Logo')
    )
    if (($assets -join ',') -cne 'Assets\StoreLogo.png,Assets\Square150x150Logo.png,Assets\Square44x44Logo.png') {
        throw 'convt identity manifest contains unexpected asset paths.'
    }
}

function Read-ConvtMenuManifest {
    param([string]$Path)
    $settings = [System.Xml.XmlReaderSettings]::new()
    $settings.DtdProcessing = [System.Xml.DtdProcessing]::Prohibit
    $settings.XmlResolver = $null
    $reader = [System.Xml.XmlReader]::Create($Path, $settings)
    try {
        $doc = [System.Xml.XmlDocument]::new()
        $doc.XmlResolver = $null
        $doc.Load($reader)
    } finally { $reader.Dispose() }
    Assert-ConvtMenuManifest -Manifest $doc
    $doc
}

function Resolve-ConvtInstallationRoot {
    param([string]$Path, [switch]$MustExist)
    if ([string]::IsNullOrWhiteSpace($Path) -or $Path -notmatch '\A(?:[A-Za-z]:[\\/]|\\\\[^\\]+\\[^\\]+\\)' -or
        $Path.IndexOfAny([char[]]"*?") -ge 0) {
        throw 'InstallationRoot must be an absolute filesystem directory without wildcards.'
    }
    $fullPath = [System.IO.Path]::GetFullPath($Path)
    $pathRoot = [System.IO.Path]::GetPathRoot($fullPath)
    $root = if ($fullPath.Length -eq $pathRoot.Length) { $pathRoot } else { $fullPath.TrimEnd('\', '/') }
    if ($MustExist -and -not [System.IO.Directory]::Exists($root)) { throw "InstallationRoot does not exist: $root" }
    $root
}

function Assert-ConvtInstalledBinary {
    param([string]$Path, [switch]$Dll)
    $item = Get-Item -LiteralPath $Path
    if ($item.PSIsContainer -or ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) {
        throw "Expected a regular convt binary: $Path"
    }
    $stream = [System.IO.File]::OpenRead($Path)
    $reader = [System.IO.BinaryReader]::new($stream)
    try {
        if ($stream.Length -lt 88 -or $reader.ReadUInt16() -ne 0x5A4D) { throw "Not a PE binary: $Path" }
        $stream.Position = 0x3C
        $offset = $reader.ReadUInt32()
        if ($offset -gt $stream.Length - 24) { throw "Malformed PE header: $Path" }
        $stream.Position = $offset
        if ($reader.ReadUInt32() -ne 0x4550 -or $reader.ReadUInt16() -ne 0x8664) { throw "Expected an x64 PE binary: $Path" }
        $stream.Position = $offset + 22
        $isDll = ($reader.ReadUInt16() -band 0x2000) -ne 0
        if ($isDll -ne [bool]$Dll) { throw "Unexpected PE binary type: $Path" }
    } finally { $reader.Dispose() }
}

function Assert-ConvtInstallation {
    param([string]$Root)
    foreach ($name in @('convt.exe', 'convt-app.exe', 'convt_shell.dll')) {
        Assert-ConvtInstalledBinary -Path (Join-Path $Root $name) -Dll:($name.EndsWith('.dll'))
    }
    $manifest = Join-Path $Root 'identity\AppxManifest.xml'
    [void](Read-ConvtMenuManifest -Path $manifest)
    foreach ($asset in @('StoreLogo.png', 'Square150x150Logo.png', 'Square44x44Logo.png')) {
        if (-not [System.IO.File]::Exists((Join-Path $Root "identity\Assets\$asset"))) { throw "Missing identity asset: $asset" }
    }
    $manifest
}

function Get-ConvtWindowsBuild {
    [int](Get-ItemPropertyValue -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -Name CurrentBuildNumber)
}

function Test-ConvtDeveloperMode {
    $key = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey('SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock')
    if (-not $key) { return $false }
    try { $key.GetValue('AllowDevelopmentWithoutDevLicense', 0) -eq 1 } finally { $key.Dispose() }
}

function Get-ConvtInstalledPackages {
    Get-AppxPackage -Name 'Convt.Desktop' -ErrorAction Stop
}

function Get-ConvtPackageExternalRoot {
    param([string]$PackageFullName)
    # Get-AppxPackage.InstallLocation is the manifest location, not ExternalLocation.
    # Query the exact current-user package using the documented EffectiveExternal
    # path type (5). It errors if no external location is associated.
    if (-not ('Convt.ModernMenu.PackagePath' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
namespace Convt.ModernMenu {
    public static class PackagePath {
        [DllImport("kernelbase.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
        public static extern int GetPackagePathByFullName2(
            string packageFullName, int pathType, ref uint pathLength, StringBuilder path);
    }
}
'@
    }
    [uint32]$length = 0
    $code = [Convt.ModernMenu.PackagePath]::GetPackagePathByFullName2($PackageFullName, 5, [ref]$length, $null)
    if ($code -ne 122 -or $length -eq 0) { throw "Cannot query convt package external location (Win32 $code)." }
    $buffer = [System.Text.StringBuilder]::new([int]$length)
    $code = [Convt.ModernMenu.PackagePath]::GetPackagePathByFullName2($PackageFullName, 5, [ref]$length, $buffer)
    if ($code -ne 0) { throw "Cannot read convt package external location (Win32 $code)." }
    $buffer.ToString()
}

function Invoke-ConvtModernComActivationProbe {
    # Run only in the disposable Probe process. Its controller owns the timeout.
    # A dedicated STA thread also works when the PowerShell host uses MTA.
    if (-not ('Convt.ModernMenu.ComActivation' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Threading;
namespace Convt.ModernMenu {
    public static class ComActivation {
        [DllImport("ole32.dll", ExactSpelling = true)]
        private static extern int CoInitializeEx(IntPtr reserved, uint flags);
        [DllImport("ole32.dll", ExactSpelling = true)]
        private static extern int CoCreateInstance(ref Guid classId, IntPtr outer,
            uint context, ref Guid interfaceId, out IntPtr instance);
        [DllImport("ole32.dll", ExactSpelling = true)]
        private static extern void CoUninitialize();

        public static void Test(Guid classId) {
            Exception failure = null;
            Thread worker = new Thread(delegate() {
                try {
                    const uint COINIT_APARTMENTTHREADED = 0x2;
                    const uint CLSCTX_LOCAL_SERVER = 0x4;
                    Guid interfaceId = new Guid("a08ce4d0-fa25-44ab-b57c-c7b1c323e0b9");
                    int initialized = CoInitializeEx(IntPtr.Zero, COINIT_APARTMENTTHREADED);
                    if (initialized < 0) {
                        throw new COMException("Modern menu COM STA initialization failed (HRESULT 0x" +
                            initialized.ToString("X8") + ").", initialized);
                    }
                    IntPtr instance = IntPtr.Zero;
                    try {
                        int result = CoCreateInstance(ref classId, IntPtr.Zero,
                            CLSCTX_LOCAL_SERVER, ref interfaceId, out instance);
                        if (result < 0) {
                            throw new COMException("Modern menu CLSCTX_LOCAL_SERVER activation of " +
                                "IID_IExplorerCommand failed (HRESULT 0x" + result.ToString("X8") + ").", result);
                        }
                        if (instance == IntPtr.Zero) {
                            throw new InvalidOperationException("Modern menu COM activation returned no interface.");
                        }
                    } finally {
                        // Release the interface while its apartment still exists.
                        try { if (instance != IntPtr.Zero) Marshal.Release(instance); }
                        finally { CoUninitialize(); }
                    }
                } catch (Exception error) { failure = error; }
            });
            worker.SetApartmentState(ApartmentState.STA);
            worker.Start();
            worker.Join();
            if (failure != null) { throw failure; }
        }
    }
}
'@
    }
    [Convt.ModernMenu.ComActivation]::Test([guid](Get-ConvtMenuIdentity).ClassId)
}

function Start-ConvtModernComActivationProcess {
    param([System.Diagnostics.ProcessStartInfo]$StartInfo)
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $StartInfo
    try {
        if (-not $process.Start()) { throw 'The modern menu COM probe process could not be started.' }
        $process
    } catch { $process.Dispose(); throw }
}

function Test-ConvtModernComActivation {
    # Registration alone does not prove Explorer can activate the surrogate.
    # Isolate native COM so a blocked activation cannot outlive compensation.
    if ([string]::IsNullOrWhiteSpace($script:ConvtMenuSourcePath) -or
        -not [System.IO.File]::Exists($script:ConvtMenuSourcePath) -or $script:ConvtMenuSourcePath.Contains('"')) {
        throw 'The modern menu COM probe requires the current helper script path.'
    }
    $windows = [System.Environment]::GetFolderPath([System.Environment+SpecialFolder]::Windows)
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = Join-Path $windows 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $classId = ([guid](Get-ConvtMenuIdentity).ClassId).ToString('D')
    # Process-scoped Bypass: the Windows client default (Restricted) refuses -File.
    $startInfo.Arguments = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + $script:ConvtMenuSourcePath + '" -Action Probe -ClassId ' + $classId
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $process = Start-ConvtModernComActivationProcess -StartInfo $startInfo
    try {
        # Drain both streams while waiting so neither pipe can block the child.
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(20000)) {
            if (-not $process.HasExited) { $process.Kill() }
            $process.WaitForExit()
            throw 'Modern menu COM activation exceeded 20 seconds; the owned probe process was stopped before compensation.'
        }
        if ($process.ExitCode -ne 0) {
            $detail = ($stderr.Result + ' ' + $stdout.Result).Trim()
            if ($detail.Length -gt 8192) { $detail = $detail.Substring(0, 8192) }
            throw "Modern menu COM activation probe failed (exit $($process.ExitCode)): $detail"
        }
    } finally {
        try {
            if (-not $process.HasExited) {
                # Even a controller/pipe failure must stop and reap this child
                # before the caller can unregister the transaction's package.
                try { $process.Kill() } finally { $process.WaitForExit() }
            }
        } finally { $process.Dispose() }
    }
}

function Assert-ConvtRegisteredPackage {
    param($Package, [string]$Root, [string]$ManifestPath, [string]$ExpectedVersion)
    $manifestRoot = Resolve-ConvtInstallationRoot -Path $Package.InstallLocation
    Assert-ConvtPackageRoot -Package $Package -Root $Root
    if ($Package.Version.ToString() -cne $ExpectedVersion -or $manifestRoot -ine (Split-Path -Parent $ManifestPath)) {
        throw 'An existing convt identity belongs to another version, manifest or external installation path; it was preserved.'
    }
}

function Assert-ConvtPackageRoot {
    param($Package, [string]$Root)
    $externalRoot = Resolve-ConvtInstallationRoot -Path (Get-ConvtPackageExternalRoot -PackageFullName $Package.PackageFullName)
    if ($externalRoot -ine $Root) { throw 'The convt identity is associated with another external installation path; it was preserved.' }
}

function Select-ConvtIdentityPackages {
    param([object[]]$Packages)
    $spec = Get-ConvtMenuIdentity
    @($Packages | Where-Object { $_.Name -ceq $spec.Name -and $_.Publisher -ceq $spec.Publisher })
}

function Register-ConvtIdentity {
    param([string]$ManifestPath, [string]$Root, [string]$SignedPath)
    if ($SignedPath) {
        Add-AppxPackage -Path $SignedPath -ExternalLocation $Root -ErrorAction Stop
    } else {
        Add-AppxPackage -Register -Path $ManifestPath -ExternalLocation $Root -ErrorAction Stop
    }
}

function Unregister-ConvtIdentity {
    param([string]$PackageFullName)
    Remove-AppxPackage -Package $PackageFullName -ErrorAction Stop
}

function Assert-ConvtLegacyRecord {
    param($Record, [string]$Root)
    $spec = Get-ConvtMenuIdentity
    $fields = @($Record.PSObject.Properties.Name | Sort-Object)
    if (($fields -join ',') -cne 'InstallationRoot,Name,Publisher,Values') { throw 'Invalid convt legacy menu backup fields.' }
    if (-not $Record -or $Record.Name -cne $spec.Name -or $Record.Publisher -cne $spec.Publisher -or
        $Record.InstallationRoot -ine $Root -or -not $Record.Values) { throw 'Invalid convt legacy menu backup identity or path.' }
    $names = @($Record.Values.PSObject.Properties.Name | Sort-Object)
    if (($names -join ',') -cne 'ExplorerCommandHandler,Icon,MUIVerb,MultiSelectModel' -or
        $Record.Values.ExplorerCommandHandler -cne ('{' + $spec.ClassId + '}') -or
        $Record.Values.MUIVerb -cne 'Convert with convt' -or $Record.Values.MultiSelectModel -cne 'Player' -or
        $Record.Values.Icon -cne ('"' + (Join-Path $Root 'convt-app.exe') + '",0')) {
        throw 'The legacy convt menu has unexpected values; it was preserved.'
    }
}

function Get-ConvtLegacyMenu {
    param([string]$Root)
    $spec = Get-ConvtMenuIdentity
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($spec.LegacyKey)
    if (-not $key) { return $null }
    try {
        if ($key.GetSubKeyNames().Count -ne 0) { throw 'The legacy convt menu has unexpected subkeys; it was preserved.' }
        $values = [ordered]@{}
        foreach ($name in $key.GetValueNames()) {
            if ($key.GetValueKind($name) -ne [Microsoft.Win32.RegistryValueKind]::String) { throw 'The legacy convt menu contains unexpected value types.' }
            $values[$name] = $key.GetValue($name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        }
        $record = [pscustomobject]@{ Name = $spec.Name; Publisher = $spec.Publisher; InstallationRoot = $Root; Values = [pscustomobject]$values }
        Assert-ConvtLegacyRecord -Record $record -Root $Root
        $record
    } finally { $key.Dispose() }
}

function Remove-ConvtLegacyMenu {
    param($Record, [string]$Root)
    $current = Get-ConvtLegacyMenu -Root $Root
    if (($current | ConvertTo-Json -Depth 4 -Compress) -cne ($Record | ConvertTo-Json -Depth 4 -Compress)) {
        throw 'The legacy convt menu changed during registration; it was preserved.'
    }
    # DeleteSubKey refuses child keys; never recursively remove a registry tree.
    $parent = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Classes\*\shell', $true)
    try { $parent.DeleteSubKey('convt', $false) } finally { if ($parent) { $parent.Dispose() } }
}

function Restore-ConvtLegacyMenu {
    param($Record, [string]$Root)
    Assert-ConvtLegacyRecord -Record $Record -Root $Root
    $current = Get-ConvtLegacyMenu -Root $Root
    if ($current) { return }
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey((Get-ConvtMenuIdentity).LegacyKey)
    try {
        foreach ($value in $Record.Values.PSObject.Properties) {
            $key.SetValue($value.Name, $value.Value, [Microsoft.Win32.RegistryValueKind]::String)
        }
    } finally { $key.Dispose() }
}

function Read-ConvtLegacyBackup {
    param([string]$Root)
    $path = Join-Path $Root 'identity\legacy-menu-backup.json'
    if (-not [System.IO.File]::Exists($path)) { return $null }
    if ((Get-Item -LiteralPath $path).Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        throw 'The convt legacy menu backup must be a regular file.'
    }
    $record = [System.IO.File]::ReadAllText($path) | ConvertFrom-Json
    Assert-ConvtLegacyRecord -Record $record -Root $Root
    $record
}

function Save-ConvtLegacyBackup {
    param($Record, [string]$Root)
    Assert-ConvtLegacyRecord -Record $Record -Root $Root
    $existing = Read-ConvtLegacyBackup -Root $Root
    if ($existing) { return }
    $path = Join-Path $Root 'identity\legacy-menu-backup.json'
    $stream = [System.IO.File]::Open($path, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    $writer = [System.IO.StreamWriter]::new($stream, [System.Text.UTF8Encoding]::new($false))
    try { $writer.Write(($Record | ConvertTo-Json -Depth 4)) } finally { $writer.Dispose() }
}

function Remove-ConvtLegacyBackup {
    param([string]$Root)
    [System.IO.File]::Delete((Join-Path $Root 'identity\legacy-menu-backup.json'))
}

function Assert-ConvtMenuTransaction {
    param($Transaction, [string]$Root)
    $spec = Get-ConvtMenuIdentity
    $fields = @($Transaction.PSObject.Properties.Name | Sort-Object)
    if (($fields -join ',') -cne 'InstallationRoot,LegacyBackupBefore,LegacyBefore,LegacyRemovalAttempted,Name,Operation,PackageBeforeFullNames,Publisher,RegistrationAttempted,RemovalAttempted,TransactionId,Version' -or
        $Transaction.Name -cne $spec.Name -or $Transaction.Publisher -cne $spec.Publisher -or
        $Transaction.InstallationRoot -ine $Root -or
        $Transaction.RegistrationAttempted -isnot [bool] -or $Transaction.LegacyRemovalAttempted -isnot [bool] -or
        $Transaction.RemovalAttempted -isnot [bool] -or $Transaction.Operation -notin @('Install', 'Remove')) {
        throw 'Invalid convt modern-menu transaction marker.'
    }
    [guid]$id = $Transaction.TransactionId
    Assert-ConvtIdentityVersion -Version $Transaction.Version
    if ($id -eq [guid]::Empty) { throw 'The convt transaction marker has no transaction ID.' }
    foreach ($name in @($Transaction.PackageBeforeFullNames)) {
        if ($name -isnot [string] -or -not $name.StartsWith($spec.Name + '_', [System.StringComparison]::Ordinal)) {
            throw 'The convt transaction marker contains an unexpected package.'
        }
    }
    if ($Transaction.LegacyBefore) { Assert-ConvtLegacyRecord -Record $Transaction.LegacyBefore -Root $Root }
    if ($Transaction.LegacyBackupBefore) { Assert-ConvtLegacyRecord -Record $Transaction.LegacyBackupBefore -Root $Root }
    if ($Transaction.LegacyRemovalAttempted -and -not $Transaction.LegacyBefore) { throw 'The convt transaction has no previous legacy registration.' }
}

function Get-ConvtTransactionPath {
    param([guid]$Id)
    if ($Id -eq [guid]::Empty) { throw 'A persisted menu transaction requires a nonempty GUID.' }
    $local = [System.Environment]::GetFolderPath([System.Environment+SpecialFolder]::LocalApplicationData)
    if (-not $local) { throw 'The current user has no local application-data directory.' }
    Join-Path (Join-Path $local 'Convt\installer-transactions') ($Id.ToString('D').ToLowerInvariant() + '.json')
}

function Read-ConvtMenuTransaction {
    param([string]$Root, [guid]$Id)
    $path = Get-ConvtTransactionPath -Id $Id
    if (-not [System.IO.File]::Exists($path)) { return $null }
    if ((Get-Item -LiteralPath $path).Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        throw 'The convt transaction marker must be a regular file.'
    }
    $record = [System.IO.File]::ReadAllText($path) | ConvertFrom-Json
    Assert-ConvtMenuTransaction -Transaction $record -Root $Root
    if ([guid]$record.TransactionId -ne $Id) { throw 'The transaction marker filename and ID disagree.' }
    $record
}

function Write-ConvtMenuTransaction {
    param($Transaction, [string]$Root)
    Assert-ConvtMenuTransaction -Transaction $Transaction -Root $Root
    $path = Get-ConvtTransactionPath -Id ([guid]$Transaction.TransactionId)
    [void][System.IO.Directory]::CreateDirectory((Split-Path -Parent $path))
    $existing = Read-ConvtMenuTransaction -Root $Root -Id ([guid]$Transaction.TransactionId)
    if ($existing -and $existing.TransactionId -cne $Transaction.TransactionId) {
        throw 'Another convt menu transaction needs Commit or Rollback before installation.'
    }
    $temporary = $path + '.' + [guid]::NewGuid().ToString('N') + '.tmp'
    $stream = [System.IO.File]::Open($temporary, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    $writer = [System.IO.StreamWriter]::new($stream, [System.Text.UTF8Encoding]::new($false))
    try {
        try { $writer.Write(($Transaction | ConvertTo-Json -Depth 6)) } finally { $writer.Dispose() }
        if ($existing) { [System.IO.File]::Replace($temporary, $path, [System.Management.Automation.Language.NullString]::Value) }
        else { [System.IO.File]::Move($temporary, $path) }
    } finally { if ([System.IO.File]::Exists($temporary)) { [System.IO.File]::Delete($temporary) } }
}

function Remove-ConvtMenuTransaction {
    param([string]$Root, [guid]$Id)
    $existing = Read-ConvtMenuTransaction -Root $Root -Id $Id
    if ($existing -and [guid]$existing.TransactionId -eq $Id) {
        [System.IO.File]::Delete((Get-ConvtTransactionPath -Id $Id))
    } elseif ($existing) { throw 'The convt menu transaction ID does not match; marker was preserved.' }
}

function Undo-ConvtMenuTransaction {
    param($Transaction, [string]$Root)
    # Appx is outside MSI's automatic rollback. Never remove a package that was
    # already present when this particular transaction began.
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($Transaction.Operation -eq 'Remove' -and $Transaction.RemovalAttempted) {
        try {
            $manifest = Assert-ConvtInstallation -Root $Root
            $document = Read-ConvtMenuManifest -Path $manifest
            if ($document.GetElementsByTagName('Identity').Item(0).GetAttribute('Version') -cne $Transaction.Version) {
                throw 'MSI rollback has not restored the previous identity manifest version.'
            }
            $packages = @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))
            if ($packages.Count -eq 0) {
                if (-not (Test-ConvtDeveloperMode)) { throw 'Developer Mode is no longer enabled; the previous loose package cannot be restored.' }
                Register-ConvtIdentity -ManifestPath $manifest -Root $Root -SignedPath $null
                $packages = @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))
            }
            if ($packages.Count -ne 1 -or @($Transaction.PackageBeforeFullNames) -notcontains $packages[0].PackageFullName) {
                throw 'The previous convt identity package could not be restored exactly.'
            }
            Assert-ConvtRegisteredPackage -Package $packages[0] -Root $Root -ManifestPath $manifest -ExpectedVersion $Transaction.Version
        } catch { $failures.Add($_.Exception.Message) }
    }
    if ($Transaction.Operation -eq 'Install' -and $Transaction.LegacyRemovalAttempted) {
        try { Restore-ConvtLegacyMenu -Record $Transaction.LegacyBefore -Root $Root }
        catch { $failures.Add($_.Exception.Message) }
    }
    if ($Transaction.Operation -eq 'Install' -and $Transaction.RegistrationAttempted) {
        $manifest = Join-Path $Root 'identity\AppxManifest.xml'
        try {
            foreach ($package in @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))) {
                if (@($Transaction.PackageBeforeFullNames) -contains $package.PackageFullName) { continue }
                Assert-ConvtRegisteredPackage -Package $package -Root $Root -ManifestPath $manifest -ExpectedVersion $Transaction.Version
                Unregister-ConvtIdentity -PackageFullName $package.PackageFullName
            }
        } catch { $failures.Add($_.Exception.Message) }
    }
    try {
        if ($Transaction.LegacyBackupBefore) { Save-ConvtLegacyBackup -Record $Transaction.LegacyBackupBefore -Root $Root }
        else { Remove-ConvtLegacyBackup -Root $Root }
    } catch { $failures.Add($_.Exception.Message) }
    if ($failures.Count -gt 0) { throw ($failures -join ' ') }
}

function Invoke-ConvtModernMenu {
    param([string]$Action, [string]$InstallationRoot, [string]$SignedPackagePath, [switch]$RestoreClassic,
          [guid]$TransactionId = [guid]::Empty)
    if ($RestoreClassic -and $Action -ne 'Remove') { throw 'RestoreClassic is supported only with Action Remove.' }
    if ($SignedPackagePath -and $Action -ne 'Install') { throw 'SignedPackagePath is supported only with Action Install.' }
    $spec = Get-ConvtMenuIdentity
    $root = $null
    if ($InstallationRoot) {
        $root = Resolve-ConvtInstallationRoot -Path $InstallationRoot -MustExist:($Action -eq 'Install' -or $RestoreClassic)
    }
    if ($Action -in @('Rollback', 'Commit')) {
        if (-not $root -or $TransactionId -eq [guid]::Empty) { throw "$Action requires InstallationRoot and TransactionId." }
        $transaction = Read-ConvtMenuTransaction -Root $root -Id $TransactionId
        if (-not $transaction) { return [pscustomobject]@{ Name = $spec.Name; Changed = $false; Action = $Action } }
        if ([guid]$transaction.TransactionId -ne $TransactionId) { throw 'The convt menu transaction ID does not match; nothing was changed.' }
        if ($Action -eq 'Rollback') { Undo-ConvtMenuTransaction -Transaction $transaction -Root $root }
        Remove-ConvtMenuTransaction -Root $root -Id $TransactionId
        return [pscustomobject]@{ Name = $spec.Name; Changed = $true; Action = $Action }
    }
    $packages = @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))
    if ($Action -eq 'Status') {
        return [pscustomobject]@{ Name = $spec.Name; Publisher = $spec.Publisher; Registered = ($packages.Count -gt 0); PackageFullNames = @($packages | ForEach-Object { $_.PackageFullName }) }
    }
    if ($Action -eq 'Remove') {
        if ($root) {
            foreach ($package in $packages) { Assert-ConvtPackageRoot -Package $package -Root $root }
        }
        $backup = $null
        if ($root -and [System.IO.Directory]::Exists($root)) { $backup = Read-ConvtLegacyBackup -Root $root }
        if ($RestoreClassic) {
            if ($TransactionId -ne [guid]::Empty) { throw 'RestoreClassic is a manual Remove option; MSI transactions restore the modern package instead.' }
            if (-not $root -or -not $backup) { throw 'RestoreClassic requires InstallationRoot and a valid legacy menu backup.' }
            [void](Assert-ConvtInstallation -Root $root)
            # Check conflicts before unregistering the modern menu.
            [void](Get-ConvtLegacyMenu -Root $root)
        }
        $transaction = $null
        if ($TransactionId -ne [guid]::Empty) {
            if (-not $root -or $packages.Count -gt 1) { throw 'Transactional Remove requires one installation root and at most one package.' }
            if (Read-ConvtMenuTransaction -Root $root -Id $TransactionId) { throw 'An earlier menu transaction needs Commit or Rollback.' }
            $manifest = Assert-ConvtInstallation -Root $root
            $document = Read-ConvtMenuManifest -Path $manifest
            $version = $document.GetElementsByTagName('Identity').Item(0).GetAttribute('Version')
            foreach ($package in $packages) { Assert-ConvtRegisteredPackage -Package $package -Root $root -ManifestPath $manifest -ExpectedVersion $version }
            if ($packages.Count -gt 0 -and -not (Test-ConvtDeveloperMode)) { throw 'Developer Mode must remain enabled to prepare safe rollback of this loose package.' }
            $transaction = [pscustomobject]@{
                Name=$spec.Name; Publisher=$spec.Publisher; InstallationRoot=$root; Version=$version
                TransactionId=$TransactionId.ToString('D').ToLowerInvariant(); Operation='Remove'
                PackageBeforeFullNames=@($packages | ForEach-Object { $_.PackageFullName })
                LegacyBefore=$null; LegacyBackupBefore=$backup
                RegistrationAttempted=$false; LegacyRemovalAttempted=$false; RemovalAttempted=$false
            }
            Write-ConvtMenuTransaction -Transaction $transaction -Root $root
        }
        try {
            foreach ($package in $packages) {
                if ($transaction) {
                    $transaction.RemovalAttempted=$true
                    Write-ConvtMenuTransaction -Transaction $transaction -Root $root
                }
                Unregister-ConvtIdentity -PackageFullName $package.PackageFullName
            }
            if ($RestoreClassic) { Restore-ConvtLegacyMenu -Record $backup -Root $root }
            if ($root -and [System.IO.Directory]::Exists($root)) { Remove-ConvtLegacyBackup -Root $root }
        } catch {
            $failure=$_.Exception.Message
            if ($transaction) {
                try {
                    Undo-ConvtMenuTransaction -Transaction $transaction -Root $root
                    Remove-ConvtMenuTransaction -Root $root -Id $TransactionId
                } catch { throw "Modern menu removal failed: $failure Rollback also failed: $($_.Exception.Message)" }
            }
            throw "Modern menu removal failed: $failure"
        }
        return [pscustomobject]@{ Name = $spec.Name; Removed = $packages.Count; ClassicRestored = [bool]$RestoreClassic }
    }
    if (-not $root) { throw 'Action Install requires InstallationRoot.' }
    if ((Get-ConvtWindowsBuild) -lt 22000) { throw 'The modern convt menu requires Windows 11 build 22000 or later.' }
    $manifest = Assert-ConvtInstallation -Root $root
    $document = Read-ConvtMenuManifest -Path $manifest
    $version = $document.GetElementsByTagName('Identity').Item(0).GetAttribute('Version')
    if ($SignedPackagePath) {
        # The current release registers a validated loose manifest. Do not deploy
        # an arbitrary signed package before its full identity validation exists.
        throw 'SignedPackagePath is reserved for signed distribution; this release supports validated loose registration only.'
    }
    if (-not (Test-ConvtDeveloperMode)) { throw 'Developer Mode must already be enabled for loose identity registration. No system setting was changed.' }
    if ($packages.Count -gt 1) { throw 'Multiple convt identity registrations were found; none was changed.' }
    foreach ($package in $packages) {
        Assert-ConvtRegisteredPackage -Package $package -Root $root -ManifestPath $manifest -ExpectedVersion $version
    }
    if ($TransactionId -ne [guid]::Empty -and (Read-ConvtMenuTransaction -Root $root -Id $TransactionId)) {
        throw 'An earlier convt menu transaction needs Commit or Rollback before installation.'
    }
    $backup = Read-ConvtLegacyBackup -Root $root
    $legacy = Get-ConvtLegacyMenu -Root $root
    if ($legacy) { Save-ConvtLegacyBackup -Record $legacy -Root $root }
    $transaction = [pscustomobject]@{
        Name = $spec.Name; Publisher = $spec.Publisher; InstallationRoot = $root
        Version = $version; TransactionId = $TransactionId.ToString('D').ToLowerInvariant(); Operation = 'Install'
        PackageBeforeFullNames = @($packages | ForEach-Object { $_.PackageFullName })
        LegacyBefore = $legacy; LegacyBackupBefore = $backup
        RegistrationAttempted = $false; LegacyRemovalAttempted = $false; RemovalAttempted = $false
    }
    if ($TransactionId -ne [guid]::Empty) { Write-ConvtMenuTransaction -Transaction $transaction -Root $root }
    try {
        if ($packages.Count -eq 0) {
            $transaction.RegistrationAttempted = $true
            if ($TransactionId -ne [guid]::Empty) { Write-ConvtMenuTransaction -Transaction $transaction -Root $root }
            Register-ConvtIdentity -ManifestPath $manifest -Root $root -SignedPath $null
        }
        $confirmed = @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))
        if ($confirmed.Count -ne 1) {
            throw 'The modern convt identity registration could not be confirmed.'
        }
        Assert-ConvtRegisteredPackage -Package $confirmed[0] -Root $root -ManifestPath $manifest -ExpectedVersion $version
        Test-ConvtModernComActivation
        if ($legacy) {
            $transaction.LegacyRemovalAttempted = $true
            if ($TransactionId -ne [guid]::Empty) { Write-ConvtMenuTransaction -Transaction $transaction -Root $root }
            Remove-ConvtLegacyMenu -Record $legacy -Root $root
        }
        [pscustomobject]@{ Name = $spec.Name; Registered = $true; PackageFullName = $confirmed[0].PackageFullName
                          ClassicRemoved = [bool]$legacy; TransactionId = $TransactionId.ToString() }
    } catch {
        $failure = $_.Exception.Message
        try {
            Undo-ConvtMenuTransaction -Transaction $transaction -Root $root
            if ($TransactionId -ne [guid]::Empty) { Remove-ConvtMenuTransaction -Root $root -Id $TransactionId }
        } catch { throw "Modern menu registration failed: $failure Rollback also failed: $($_.Exception.Message)" }
        throw "Modern menu registration failed: $failure"
    }
}

function Invoke-ConvtMenuHelper {
    param([ValidateSet('Install', 'Remove', 'Status', 'Rollback', 'Commit', 'Probe')][string]$Action,
          [string]$InstallationRoot, [string]$SignedPackagePath, [switch]$RestoreClassic,
          [guid]$TransactionId = [guid]::Empty, [guid]$ClassId = [guid]::Empty)
    if ($Action -eq 'Probe') {
        if ($ClassId -ne [guid](Get-ConvtMenuIdentity).ClassId -or $InstallationRoot -or
            $SignedPackagePath -or $RestoreClassic -or $TransactionId -ne [guid]::Empty) {
            throw 'Action Probe accepts only the fixed convt COM class ID and no package lifecycle options.'
        }
        Invoke-ConvtModernComActivationProbe
        return [pscustomobject]@{ ProbePassed = $true }
    }
    if ($ClassId -ne [guid]::Empty) { throw 'ClassId is supported only by Action Probe.' }
    Invoke-ConvtModernMenu -Action $Action -InstallationRoot $InstallationRoot -SignedPackagePath $SignedPackagePath -RestoreClassic:$RestoreClassic -TransactionId $TransactionId
}

Invoke-ConvtMenuHelper -Action $Action -InstallationRoot $InstallationRoot -SignedPackagePath $SignedPackagePath -RestoreClassic:$RestoreClassic -TransactionId $TransactionId -ClassId $ClassId |
    ConvertTo-Json -Depth 6
