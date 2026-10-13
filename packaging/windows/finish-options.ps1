# Physical installer integrations only. User settings/presets are never edited.
[CmdletBinding()]
param(
    [ValidateSet('Prepare', 'Apply', 'Configure', 'Remove', 'Rollback', 'Commit')]
    [string]$Action,
    [string]$InstallationRoot,
    [string]$PreviousInstallationRoot,
    [ValidateSet('0', '1')][string]$ContextMenu = '1',
    [ValidateSet('0', '1')][string]$DesktopShortcut = '0',
    [guid]$TransactionId,
    [switch]$HoldTransaction,
    [string]$CoreModernScript,
    [string]$ResultPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Import-ConvtMenuFunctions {
    param([string]$Path)
    $tokens = $null; $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($Path, [ref]$tokens, [ref]$errors)
    if ($errors) { throw 'The embedded modern menu helper has syntax errors.' }
    # Import declarations, never its CLI entry point or parameter defaults.
    foreach ($function in $ast.EndBlock.Statements) {
        if ($function -is [System.Management.Automation.Language.FunctionDefinitionAst]) {
            . ([scriptblock]::Create($function.Extent.Text))
            Set-Item -Path ('Function:script:' + $function.Name) -Value (Get-Item -Path ('Function:' + $function.Name)).ScriptBlock
        }
    }
    $script:ConvtMenuSourcePath = $Path
}

function Get-ConvtFinishJournalPath {
    param([guid]$Id)
    if ($Id -eq [guid]::Empty) { throw 'Installer configuration requires a nonempty transaction GUID.' }
    Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)) (
        'Convt\installer-transactions\' + $Id.ToString('D').ToLowerInvariant() + '.finish.json')
}

function Get-ConvtFinishPackageFiles {
    @('convt.exe', 'convt-app.exe', 'convt_shell.dll', 'identity/AppxManifest.xml',
      'identity/Assets/Square150x150Logo.png', 'identity/Assets/Square44x44Logo.png', 'identity/Assets/StoreLogo.png')
}

function Get-ConvtFinishBackupRoot {
    param([guid]$Id)
    Join-Path (Split-Path -Parent (Get-ConvtFinishJournalPath -Id $Id)) ($Id.ToString('D').ToLowerInvariant() + '.finish-backup')
}

function Assert-ConvtRegularPathParents {
    param([string]$Path)
    $current = [IO.Path]::GetFullPath($Path)
    while ($current) {
        $item = Get-Item -LiteralPath $current -ErrorAction SilentlyContinue
        if ($item -and ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Installer snapshot paths must not traverse reparse points.'
        }
        $parent = [IO.Path]::GetDirectoryName($current)
        if ($parent -eq $current) { break }
        $current = $parent
    }
}

function Assert-ConvtSnapshotFile {
    param([string]$Path, $Record)
    Assert-ConvtRegularPathParents -Path $Path
    $item = Get-Item -LiteralPath $Path
    if ($item.PSIsContainer -or $item.Length -ne $Record.Length -or
        (Get-ConvtSnapshotHash -Path $Path) -cne $Record.Sha256) {
        throw 'A package snapshot file changed; it was preserved.'
    }
}

function Get-ConvtSnapshotHash {
    param([string]$Path)
    $stream = [IO.File]::OpenRead($Path)
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { [BitConverter]::ToString($hasher.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $hasher.Dispose(); $stream.Dispose() }
}

function Backup-ConvtFinishPackageFiles {
    param([string]$Root, [guid]$Id)
    $backup = Get-ConvtFinishBackupRoot -Id $Id
    Assert-ConvtRegularPathParents -Path $backup
    if (Test-Path -LiteralPath $backup) { throw 'A package snapshot directory already exists; it was preserved.' }
    $records = @()
    foreach ($relative in Get-ConvtFinishPackageFiles) {
        $source = Join-Path $Root $relative
        Assert-ConvtRegularPathParents -Path $source
        $item = Get-Item -LiteralPath $source
        if ($item.PSIsContainer -or $item.Length -le 0) { throw 'Expected a nonempty regular package snapshot source.' }
        $records += [pscustomobject]@{Path=$relative;Length=[long]$item.Length
            Sha256=(Get-ConvtSnapshotHash -Path $source)}
    }
    try {
        foreach ($record in $records) {
            $source = Join-Path $Root $record.Path
            $destination = Join-Path $backup $record.Path
            [void][IO.Directory]::CreateDirectory((Split-Path -Parent $destination))
            Assert-ConvtRegularPathParents -Path $destination
            $temporary = $destination + '.' + [guid]::NewGuid().ToString('N') + '.tmp'
            try {
                [IO.File]::Copy($source, $temporary, $false)
                Assert-ConvtSnapshotFile -Path $temporary -Record $record
                [IO.File]::Move($temporary, $destination)
            } finally { if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) } }
        }
    } catch {
        $failure = $_.Exception.Message
        try { Remove-ConvtFinishPackageBackup -Journal ([pscustomobject]@{BeforeFiles=$records;TransactionId=$Id.ToString('D')}) }
        catch { throw "Package snapshot failed: $failure Snapshot cleanup failed: $($_.Exception.Message)" }
        throw "Package snapshot failed: $failure"
    }
    $records
}

function Restore-ConvtFinishPackageFiles {
    param($Journal)
    $files = @($Journal.BeforeFiles)
    if ($files.Count -eq 0) { return }
    $backup = Get-ConvtFinishBackupRoot -Id ([guid]$Journal.TransactionId)
    # Validate every source and destination before creating any file. These
    # seven allowlisted files let normal Appx registration restore the old
    # identity while MSI has yet to restore the rest of its old payload.
    foreach ($file in $files) {
        Assert-ConvtSnapshotFile -Path (Join-Path $backup $file.Path) -Record $file
        $destination = Join-Path $Journal.BeforeRoot $file.Path
        Assert-ConvtRegularPathParents -Path $destination
        if (Test-Path -LiteralPath $destination) { Assert-ConvtSnapshotFile -Path $destination -Record $file }
    }
    foreach ($file in $files) {
        $destination = Join-Path $Journal.BeforeRoot $file.Path
        if (-not [IO.File]::Exists($destination)) {
            [void][IO.Directory]::CreateDirectory((Split-Path -Parent $destination))
            [IO.File]::Copy((Join-Path $backup $file.Path), $destination, $false)
            Assert-ConvtSnapshotFile -Path $destination -Record $file
        }
    }
}

function Remove-ConvtFinishPackageBackup {
    param($Journal)
    $files = @($Journal.BeforeFiles)
    if ($files.Count -eq 0) { return }
    $backup = Get-ConvtFinishBackupRoot -Id ([guid]$Journal.TransactionId)
    Assert-ConvtRegularPathParents -Path $backup
    $allowedFiles = @(Get-ConvtFinishPackageFiles)
    $allowedDirectories = @('identity', 'identity/Assets')
    # Commit failures can trigger MSI rollback. Refuse foreign contents before
    # deleting any part of the recovery snapshot, rather than discovering them
    # only while removing its directories.
    foreach ($relative in @('', 'identity', 'identity/Assets')) {
        $directory = if ($relative) { Join-Path $backup $relative } else { $backup }
        if (-not [IO.Directory]::Exists($directory)) { continue }
        Assert-ConvtRegularPathParents -Path $directory
        foreach ($item in Get-ChildItem -LiteralPath $directory -Force) {
            $name = $item.FullName.Substring($backup.Length + 1).Replace('\', '/')
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
                ($item.PSIsContainer -and $name -notin $allowedDirectories) -or
                (-not $item.PSIsContainer -and $name -notin $allowedFiles)) {
                throw 'The package snapshot contains foreign contents; all files were preserved.'
            }
        }
    }
    foreach ($file in $files) {
        $path = Join-Path $backup $file.Path
        if (Test-Path -LiteralPath $path) { Assert-ConvtSnapshotFile -Path $path -Record $file }
    }
    foreach ($file in $files) {
        $path = Join-Path $backup $file.Path
        if ([IO.File]::Exists($path)) { [IO.File]::Delete($path) }
    }
    # Empty-directory removal refuses foreign files/subtrees. Never recurse.
    foreach ($relative in @('identity/Assets', 'identity', '')) {
        $path = if ($relative) { Join-Path $backup $relative } else { $backup }
        if ([IO.Directory]::Exists($path)) { [IO.Directory]::Delete($path, $false) }
    }
}

function Assert-ConvtFinishJournal {
    param($Record, [string]$Root, [guid]$Id)
    $fields = @($Record.PSObject.Properties.Name | Sort-Object) -join ','
    if ($fields -cne 'Attempted,BeforeFiles,BeforePackage,BeforeRoot,InstallationRoot,LegacyBackupBefore,LegacyBefore,Name,Operation,PreferencesBefore,Publisher,ShortcutBefore,TransactionId' -or
        $Record.Name -cne 'Convt.Desktop' -or $Record.Publisher -cne 'CN=Convt Local' -or
        $Record.InstallationRoot -ine $Root -or [guid]$Record.TransactionId -ne $Id -or
        $Record.Operation -notin @('Apply', 'Configure', 'Remove') -or $Record.Attempted -isnot [bool]) {
        throw 'Invalid installer integration transaction; it was preserved.'
    }
    $beforeRoot = Resolve-ConvtInstallationRoot -Path $Record.BeforeRoot
    $files = @($Record.BeforeFiles)
    if ($files.Count -ne 0) {
        if (-not $Record.BeforePackage -or $Record.Operation -eq 'Configure' -or
            $files.Count -ne 7 -or
            (@($files | ForEach-Object {$_.Path} | Sort-Object) -join ',') -cne (@(Get-ConvtFinishPackageFiles | Sort-Object) -join ',')) {
            throw 'Unexpected installer package snapshot files.'
        }
        foreach ($file in $files) {
            if ((@($file.PSObject.Properties.Name | Sort-Object) -join ',') -cne 'Length,Path,Sha256' -or
                ($file.Length -isnot [long] -and $file.Length -isnot [int]) -or [long]$file.Length -le 0 -or
                $file.Sha256 -notmatch '\A[a-f0-9]{64}\z') { throw 'Invalid installer package snapshot metadata.' }
        }
    } elseif ($Record.BeforePackage -and $Record.Operation -ne 'Configure') {
        throw 'The previous package requires its validated file snapshot.'
    }
    if ($Record.LegacyBefore) { Assert-ConvtLegacyRecord -Record $Record.LegacyBefore -Root $beforeRoot }
    if ($Record.LegacyBackupBefore) { Assert-ConvtLegacyRecord -Record $Record.LegacyBackupBefore -Root $beforeRoot }
    if ($Record.BeforePackage) {
        $package = $Record.BeforePackage
        if ((@($package.PSObject.Properties.Name | Sort-Object) -join ',') -cne 'FullName,Manifest,Root,Version' -or
            $package.Root -ine $beforeRoot -or $package.FullName -notmatch '\AConvt\.Desktop_[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+_x64__[a-z0-9]+\z') {
            throw 'Invalid previous package snapshot.'
        }
        Assert-ConvtIdentityVersion -Version $package.Version
        if (-not $package.FullName.StartsWith(('Convt.Desktop_' + $package.Version + '_x64__'), [StringComparison]::Ordinal)) {
            throw 'Previous package full name and version disagree.'
        }
        $manifestBytes = [Convert]::FromBase64String($package.Manifest)
        if ($files.Count -gt 0) {
            $savedManifest = @($files | Where-Object Path -ceq 'identity/AppxManifest.xml')[0]
            $hasher = [Security.Cryptography.SHA256]::Create()
            try { $manifestHash = [BitConverter]::ToString($hasher.ComputeHash($manifestBytes)).Replace('-', '').ToLowerInvariant() }
            finally { $hasher.Dispose() }
            if ($savedManifest.Length -ne $manifestBytes.LongLength -or $savedManifest.Sha256 -cne $manifestHash) {
                throw 'The previous manifest bytes and file snapshot disagree.'
            }
        }
        $settings = [Xml.XmlReaderSettings]::new()
        $settings.DtdProcessing = [Xml.DtdProcessing]::Prohibit
        $settings.XmlResolver = $null
        $text = [IO.StringReader]::new([Text.Encoding]::UTF8.GetString($manifestBytes))
        $reader = [Xml.XmlReader]::Create($text, $settings)
        try {
            $document = [Xml.XmlDocument]::new()
            $document.XmlResolver = $null
            $document.Load($reader)
        } finally { $reader.Dispose(); $text.Dispose() }
        Assert-ConvtMenuManifest -Manifest $document
        if ($document.GetElementsByTagName('Identity').Item(0).GetAttribute('Version') -cne $package.Version) {
            throw 'Previous package version and snapshot disagree.'
        }
    }
    if ($Record.ShortcutBefore) {
        if ((@($Record.ShortcutBefore.PSObject.Properties.Name | Sort-Object) -join ',') -cne 'Bytes,Root' -or
            $Record.ShortcutBefore.Root -ine $beforeRoot) { throw 'Invalid shortcut snapshot.' }
        [void][Convert]::FromBase64String($Record.ShortcutBefore.Bytes)
    }
    if ($Record.PreferencesBefore) { Assert-ConvtInstallerPreferences -Record $Record.PreferencesBefore -Root $beforeRoot }
}

function Read-ConvtFinishJournal {
    param([string]$Root, [guid]$Id)
    $path = Get-ConvtFinishJournalPath -Id $Id
    if (-not [IO.File]::Exists($path)) { return $null }
    if ((Get-Item -LiteralPath $path).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Installer journal must be a regular file.' }
    $record = [IO.File]::ReadAllText($path) | ConvertFrom-Json
    Assert-ConvtFinishJournal -Record $record -Root $Root -Id $Id
    $record
}

function Write-ConvtFinishJournal {
    param($Record, [string]$Root, [guid]$Id)
    Assert-ConvtFinishJournal -Record $Record -Root $Root -Id $Id
    $path = Get-ConvtFinishJournalPath -Id $Id
    $parent = Split-Path -Parent $path
    [void][IO.Directory]::CreateDirectory($parent)
    if ((Get-Item -LiteralPath $parent).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Installer journal directory must not be a reparse point.' }
    $existing = Read-ConvtFinishJournal -Root $Root -Id $Id
    $temporary = $path + '.' + [guid]::NewGuid().ToString('N') + '.tmp'
    try {
        [IO.File]::WriteAllText($temporary, ($Record | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
        if ($existing) { [IO.File]::Replace($temporary, $path, [System.Management.Automation.Language.NullString]::Value) }
        else { [IO.File]::Move($temporary, $path) }
    } finally { if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) } }
}

function Remove-ConvtFinishJournal {
    param([string]$Root, [guid]$Id)
    $journal = Read-ConvtFinishJournal -Root $Root -Id $Id
    if ($journal) {
        Remove-ConvtFinishPackageBackup -Journal $journal
        [IO.File]::Delete((Get-ConvtFinishJournalPath -Id $Id))
    }
}

function Assert-ConvtInstallerPreferences {
    param($Record, [string]$Root)
    if ((@($Record.PSObject.Properties.Name | Sort-Object) -join ',') -cne 'ContextMenu,DesktopShortcut,InstallationRoot,Name,Publisher' -or
        $Record.Name -cne 'Convt.Desktop' -or $Record.Publisher -cne 'CN=Convt Local' -or
        $Record.InstallationRoot -ine $Root -or $Record.ContextMenu -notin @('0', '1') -or $Record.DesktopShortcut -notin @('0', '1')) {
        throw 'Installer preferences belong to another installation or have unexpected values.'
    }
}

function Open-ConvtInstallerPreferencesKey {
    [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Convt\InstallerOptions')
}

function Get-ConvtInstallerPreferences {
    param([string]$Root)
    $key = Open-ConvtInstallerPreferencesKey
    if (-not $key) { return $null }
    try {
        $names = @($key.GetValueNames())
        # Removing our values intentionally preserves this key's foreign
        # subkeys. An empty key therefore means no installer preferences.
        if ($names.Count -eq 0) { return $null }
        $values = [ordered]@{}
        foreach ($name in $names) {
            if ($key.GetValueKind($name) -ne [Microsoft.Win32.RegistryValueKind]::String) { throw 'Unexpected installer preference value type.' }
            $values[$name] = $key.GetValue($name)
        }
        $record = [pscustomobject]$values
        Assert-ConvtInstallerPreferences -Record $record -Root $Root
        $record
    } finally { $key.Dispose() }
}

function Set-ConvtInstallerPreferences {
    param($Record, [string]$Root)
    Assert-ConvtInstallerPreferences -Record $Record -Root $Root
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Software\Convt\InstallerOptions')
    try {
        foreach ($property in $Record.PSObject.Properties) { $key.SetValue($property.Name, [string]$property.Value, [Microsoft.Win32.RegistryValueKind]::String) }
    } finally { $key.Dispose() }
}

function Remove-ConvtInstallerPreferences {
    param([string]$Root)
    $record = Get-ConvtInstallerPreferences -Root $Root
    if (-not $record) { return }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Convt\InstallerOptions', $true)
    try { foreach ($name in $record.PSObject.Properties.Name) { $key.DeleteValue($name, $false) } }
    finally { $key.Dispose() }
}

function Get-ConvtDesktopShortcutPath {
    Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::DesktopDirectory)) 'convt.lnk'
}

function Get-ConvtOwnedDesktopShortcut {
    param([string]$Root, [switch]$RequireOwned)
    $path = Get-ConvtDesktopShortcutPath
    if (-not [IO.File]::Exists($path)) { return $null }
    $item = Get-Item -LiteralPath $path
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'The desktop shortcut must not be a reparse point.' }
    $shell = New-Object -ComObject WScript.Shell
    try {
        $shortcut = $shell.CreateShortcut($path)
        try {
            $owned = $shortcut.TargetPath -ieq (Join-Path $Root 'convt-app.exe') -and
                [string]::IsNullOrEmpty($shortcut.Arguments) -and $shortcut.WorkingDirectory -ieq $Root
            if (-not $owned) {
                if ($RequireOwned) { throw 'The existing convt.lnk points to another application; it was preserved.' }
                return $null
            }
            [pscustomobject]@{ Root=$Root; Bytes=[Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) }
        } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut) }
    } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shell) }
}

function Set-ConvtOwnedDesktopShortcut {
    param([string]$Root)
    [void](Get-ConvtOwnedDesktopShortcut -Root $Root -RequireOwned)
    $shell = New-Object -ComObject WScript.Shell
    try {
        $shortcut = $shell.CreateShortcut((Get-ConvtDesktopShortcutPath))
        try {
            $shortcut.TargetPath = Join-Path $Root 'convt-app.exe'
            $shortcut.WorkingDirectory = $Root
            $shortcut.Arguments = ''
            $shortcut.IconLocation = ('"' + (Join-Path $Root 'convt-app.exe') + '",0')
            $shortcut.Description = 'convt'
            $shortcut.Save()
        } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut) }
    } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shell) }
}

function Remove-ConvtOwnedDesktopShortcut {
    param([string]$Root)
    if (Get-ConvtOwnedDesktopShortcut -Root $Root) { [IO.File]::Delete((Get-ConvtDesktopShortcutPath)) }
}

function New-ConvtLegacyRecord {
    param([string]$Root)
    [pscustomobject]@{ Name='Convt.Desktop'; Publisher='CN=Convt Local'; InstallationRoot=$Root
        Values=[pscustomobject]@{MUIVerb='Convert with convt'; ExplorerCommandHandler='{710FB9A8-C47E-4B39-9CFA-E273AB1B78F8}'
            MultiSelectModel='Player'; Icon=('"' + (Join-Path $Root 'convt-app.exe') + '",0')} }
}

function Get-ConvtFinishPackageSnapshot {
    param([string]$Root)
    $packages = @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))
    if ($packages.Count -gt 1) { throw 'Multiple convt identity packages were found; none was changed.' }
    if ($packages.Count -eq 0) { return $null }
    $package = $packages[0]
    $manifest = Assert-ConvtInstallation -Root $Root
    Assert-ConvtRegisteredPackage -Package $package -Root $Root -ManifestPath $manifest -ExpectedVersion $package.Version.ToString()
    $document = Read-ConvtMenuManifest -Path $manifest
    if ($document.GetElementsByTagName('Identity').Item(0).GetAttribute('Version') -cne $package.Version.ToString()) {
        throw 'Snapshot must run before the previous package manifest is replaced.'
    }
    [pscustomobject]@{FullName=$package.PackageFullName; Root=$Root; Version=$package.Version.ToString()
        Manifest=[Convert]::ToBase64String([IO.File]::ReadAllBytes($manifest))}
}

function Assert-ConvtUnattemptedBaseline {
    param($Journal)
    if ($Journal.Attempted -or $Journal.Operation -ne 'Apply') { throw 'An attempted integration transaction requires explicit rollback or commit.' }
    $root = $Journal.BeforeRoot
    $package = Get-ConvtFinishPackageSnapshot -Root $root
    if ([bool]$package -ne [bool]$Journal.BeforePackage) { throw 'The package baseline changed; the unfinished transaction was preserved.' }
    if ($package) {
        foreach ($name in @('FullName', 'Manifest', 'Root', 'Version')) {
            if ($package.$name -cne $Journal.BeforePackage.$name) { throw 'The package baseline changed; the unfinished transaction was preserved.' }
        }
    }
    foreach ($file in @($Journal.BeforeFiles)) { Assert-ConvtSnapshotFile -Path (Join-Path $root $file.Path) -Record $file }
    # Both legacy records validate the same fixed values for this root. Presence
    # is sufficient once each has passed the strict ownership/schema checks.
    $legacy = Get-ConvtLegacyMenu -Root $root
    $backup = Read-ConvtLegacyBackup -Root $root
    if ([bool]$legacy -ne [bool]$Journal.LegacyBefore -or [bool]$backup -ne [bool]$Journal.LegacyBackupBefore) {
        throw 'The menu baseline changed; the unfinished transaction was preserved.'
    }
    $preferences = Get-ConvtInstallerPreferences -Root $root
    if ([bool]$preferences -ne [bool]$Journal.PreferencesBefore) { throw 'The installer preference baseline changed; the unfinished transaction was preserved.' }
    if ($preferences -and ($preferences.ContextMenu -cne $Journal.PreferencesBefore.ContextMenu -or
        $preferences.DesktopShortcut -cne $Journal.PreferencesBefore.DesktopShortcut)) {
        throw 'The installer preference baseline changed; the unfinished transaction was preserved.'
    }
    $shortcut = Get-ConvtOwnedDesktopShortcut -Root $root
    if ([bool]$shortcut -ne [bool]$Journal.ShortcutBefore -or
        ($shortcut -and ($shortcut.Bytes -cne $Journal.ShortcutBefore.Bytes -or $shortcut.Root -ine $Journal.ShortcutBefore.Root))) {
        throw 'The desktop shortcut baseline changed; the unfinished transaction was preserved.'
    }
}

function New-ConvtFinishJournal {
    param([string]$Root, [string]$PreviousRoot, [guid]$Id, [string]$Operation)
    $existing = Read-ConvtFinishJournal -Root $Root -Id $Id
    if ($existing) {
        if ($Operation -ne 'Apply') { throw 'An earlier installer integration transaction needs rollback or commit.' }
        # Early RemoveExistingProducts can abort before the rollback CA is
        # scheduled. Retry only if no integration mutation was attempted and
        # the entire validated previous baseline has already been restored.
        Assert-ConvtUnattemptedBaseline -Journal $existing
        Remove-ConvtFinishJournal -Root $Root -Id $Id
    }
    $beforeRoot = if ($PreviousRoot) { $PreviousRoot } else { $Root }
    $record = [pscustomobject]@{ Name='Convt.Desktop'; Publisher='CN=Convt Local'; InstallationRoot=$Root
        BeforeRoot=$beforeRoot; TransactionId=$Id.ToString('D').ToLowerInvariant(); Operation=$Operation; Attempted=$false
        BeforeFiles=@()
        BeforePackage=(Get-ConvtFinishPackageSnapshot -Root $beforeRoot)
        LegacyBefore=(Get-ConvtLegacyMenu -Root $beforeRoot)
        LegacyBackupBefore=(Read-ConvtLegacyBackup -Root $beforeRoot)
        ShortcutBefore=(Get-ConvtOwnedDesktopShortcut -Root $beforeRoot)
        PreferencesBefore=(Get-ConvtInstallerPreferences -Root $beforeRoot) }
    if ($record.BeforePackage -and $Operation -ne 'Configure') {
        $record.BeforeFiles = @(Backup-ConvtFinishPackageFiles -Root $beforeRoot -Id $Id)
    }
    $record
}

function Assert-ConvtFinishPackageOwnership {
    param($Package, $Journal)
    $root = Resolve-ConvtInstallationRoot -Path (Get-ConvtPackageExternalRoot -PackageFullName $Package.PackageFullName)
    if ($root -ine $Journal.InstallationRoot -and
        (-not $Journal.BeforePackage -or $Package.PackageFullName -cne $Journal.BeforePackage.FullName -or $root -ine $Journal.BeforePackage.Root)) {
        throw 'The convt package belongs to another installation; it was preserved.'
    }
    $manifestRoot = Resolve-ConvtInstallationRoot -Path $Package.InstallLocation
    if ($manifestRoot -ine (Join-Path $root 'identity')) { throw 'Unexpected convt package manifest location; it was preserved.' }
}

function Get-ConvtJournalLegacyMenu {
    param($Journal)
    try { Get-ConvtLegacyMenu -Root $Journal.InstallationRoot }
    catch {
        if ($Journal.BeforeRoot -ieq $Journal.InstallationRoot) { throw }
        Get-ConvtLegacyMenu -Root $Journal.BeforeRoot
    }
}

function Get-ConvtJournalPreferences {
    param($Journal)
    try { Get-ConvtInstallerPreferences -Root $Journal.InstallationRoot }
    catch {
        if ($Journal.BeforeRoot -ieq $Journal.InstallationRoot) { throw }
        Get-ConvtInstallerPreferences -Root $Journal.BeforeRoot
    }
}

function Undo-ConvtFinishJournal {
    param($Journal)
    $root = $Journal.InstallationRoot
    $failures = [Collections.Generic.List[string]]::new()
    try {
        $packages = @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))
        foreach ($package in $packages) {
            Assert-ConvtFinishPackageOwnership -Package $package -Journal $Journal
            if ($Journal.Attempted -and (-not $Journal.BeforePackage -or $package.PackageFullName -cne $Journal.BeforePackage.FullName)) {
                Unregister-ConvtIdentity -PackageFullName $package.PackageFullName
            }
        }
        if ($Journal.BeforePackage) {
            Restore-ConvtFinishPackageFiles -Journal $Journal
            $packages = @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))
            if (@($packages | ForEach-Object { $_.PackageFullName }) -notcontains $Journal.BeforePackage.FullName) {
                $before = $Journal.BeforePackage
                $manifest = Assert-ConvtInstallation -Root $before.Root
                if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($manifest)) -cne $before.Manifest) {
                    throw 'The previous manifest and transaction snapshot disagree.'
                }
                Register-ConvtIdentity -ManifestPath $manifest -Root $before.Root -SignedPath $null
                $packages = @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))
                if ($packages.Count -ne 1 -or $packages[0].PackageFullName -cne $before.FullName) { throw 'The previous package could not be restored exactly.' }
            }
        }
    } catch { $failures.Add($_.Exception.Message) }
    # Early removal can fail before Apply starts. Restore the package files
    # needed by its preserved identity, but leave other MSI-managed state to
    # MSI's rollback when this helper never mutated those integrations.
    if (-not $Journal.Attempted) {
        if ($failures.Count -gt 0) { throw ($failures -join ' ') }
        return
    }
    try {
        $current = Get-ConvtJournalLegacyMenu -Journal $Journal
        if ($current -and ($current | ConvertTo-Json -Depth 5 -Compress) -cne ($Journal.LegacyBefore | ConvertTo-Json -Depth 5 -Compress)) {
            Remove-ConvtLegacyMenu -Record $current -Root $current.InstallationRoot
        }
        if ($Journal.LegacyBefore) { Restore-ConvtLegacyMenu -Record $Journal.LegacyBefore -Root $Journal.BeforeRoot }
        if ($Journal.LegacyBackupBefore) { Save-ConvtLegacyBackup -Record $Journal.LegacyBackupBefore -Root $Journal.BeforeRoot }
        if ($Journal.BeforeRoot -ine $root -or -not $Journal.LegacyBackupBefore) {
            # A folder transfer can have an old backup to restore and a new
            # backup to discard. Validate the latter's ownership before removal.
            if (Read-ConvtLegacyBackup -Root $root) { Remove-ConvtLegacyBackup -Root $root }
        }
    } catch { $failures.Add($_.Exception.Message) }
    try {
        Remove-ConvtOwnedDesktopShortcut -Root $root
        if ($Journal.ShortcutBefore) {
            [void](Get-ConvtOwnedDesktopShortcut -Root $Journal.ShortcutBefore.Root -RequireOwned)
            [IO.File]::WriteAllBytes((Get-ConvtDesktopShortcutPath), [Convert]::FromBase64String($Journal.ShortcutBefore.Bytes))
        }
    } catch { $failures.Add($_.Exception.Message) }
    try {
        $current = Get-ConvtJournalPreferences -Journal $Journal
        if ($current) { Remove-ConvtInstallerPreferences -Root $current.InstallationRoot }
        if ($Journal.PreferencesBefore) { Set-ConvtInstallerPreferences -Record $Journal.PreferencesBefore -Root $Journal.BeforeRoot }
    } catch { $failures.Add($_.Exception.Message) }
    if ($failures.Count -gt 0) { throw ($failures -join ' ') }
}

function Apply-ConvtFinishOptions {
    param($Journal, [bool]$Menu, [bool]$Desktop, [switch]$Removing)
    $root = $Journal.InstallationRoot
    $manifest = Assert-ConvtInstallation -Root $root
    $document = Read-ConvtMenuManifest -Path $manifest
    $version = $document.GetElementsByTagName('Identity').Item(0).GetAttribute('Version')
    $oldShortcut = $null
    if ($Journal.ShortcutBefore -and $Journal.ShortcutBefore.Root -ine $root) {
        $oldShortcut = Get-ConvtOwnedDesktopShortcut -Root $Journal.ShortcutBefore.Root
        if ($oldShortcut -and $oldShortcut.Bytes -cne $Journal.ShortcutBefore.Bytes) {
            throw 'The old desktop shortcut changed after the snapshot; it was preserved.'
        }
    }
    if ($Desktop -and -not $oldShortcut) { [void](Get-ConvtOwnedDesktopShortcut -Root $root -RequireOwned) }
    $packages = @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))
    foreach ($package in $packages) { Assert-ConvtFinishPackageOwnership -Package $package -Journal $Journal }
    if ($packages.Count -gt 1) { throw 'Multiple convt identity packages were found.' }
    $Journal.Attempted = $true
    Write-ConvtFinishJournal -Record $Journal -Root $root -Id ([guid]$Journal.TransactionId)
    if ($oldShortcut) { Remove-ConvtOwnedDesktopShortcut -Root $oldShortcut.Root }
    if ($Menu) {
        $legacy = New-ConvtLegacyRecord -Root $root
        Restore-ConvtLegacyMenu -Record $legacy -Root $root
        if ((Get-ConvtWindowsBuild) -ge 22000 -and (Test-ConvtDeveloperMode)) {
            foreach ($package in $packages) {
                if ($package.Version.ToString() -cne $version -or $package.InstallLocation -ine (Join-Path $root 'identity')) {
                    Unregister-ConvtIdentity -PackageFullName $package.PackageFullName
                }
            }
            $remaining = @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))
            if ($remaining.Count -eq 0) { Register-ConvtIdentity -ManifestPath $manifest -Root $root -SignedPath $null }
            $confirmed = @(Select-ConvtIdentityPackages -Packages @(Get-ConvtInstalledPackages))
            if ($confirmed.Count -ne 1) { throw 'The modern package registration could not be confirmed.' }
            Assert-ConvtRegisteredPackage -Package $confirmed[0] -Root $root -ManifestPath $manifest -ExpectedVersion $version
            Test-ConvtModernComActivation
            Save-ConvtLegacyBackup -Record $legacy -Root $root
            Remove-ConvtLegacyMenu -Record $legacy -Root $root
        } else {
            # Keep one physical menu when modern registration is unavailable.
            # These packages were already validated against this transaction's
            # current/previous roots; never leave an old-root provider behind.
            foreach ($package in $packages) { Unregister-ConvtIdentity -PackageFullName $package.PackageFullName }
        }
    } else {
        foreach ($package in $packages) { Unregister-ConvtIdentity -PackageFullName $package.PackageFullName }
        $legacy = Get-ConvtLegacyMenu -Root $root
        if ($legacy) { Remove-ConvtLegacyMenu -Record $legacy -Root $root }
        Remove-ConvtLegacyBackup -Root $root
    }
    if ($Desktop) { Set-ConvtOwnedDesktopShortcut -Root $root }
    else { Remove-ConvtOwnedDesktopShortcut -Root $root }
    if ($Removing) { Remove-ConvtInstallerPreferences -Root $root }
    else {
        Set-ConvtInstallerPreferences -Root $root -Record ([pscustomobject]@{
            Name='Convt.Desktop'; Publisher='CN=Convt Local'; InstallationRoot=$root
            ContextMenu=([int]$Menu).ToString(); DesktopShortcut=([int]$Desktop).ToString() })
    }
}

function Invoke-ConvtFinishOptions {
    param([string]$Action, [string]$Root, [string]$PreviousRoot, [bool]$Menu, [bool]$Desktop,
          [guid]$Id, [switch]$HoldTransaction)
    $root = Resolve-ConvtInstallationRoot -Path $Root
    $previous = if ($PreviousRoot -and $Action -ne 'Configure') { Resolve-ConvtInstallationRoot -Path $PreviousRoot } else { $null }
    $journal = Read-ConvtFinishJournal -Root $root -Id $Id
    if ($Action -eq 'Commit') { Remove-ConvtFinishJournal -Root $root -Id $Id; return }
    if ($Action -eq 'Rollback') {
        if ($journal) { Undo-ConvtFinishJournal -Journal $journal; Remove-ConvtFinishJournal -Root $root -Id $Id }
        return
    }
    if ($Action -in @('Prepare', 'Configure')) {
        $operation = if ($Action -eq 'Configure') { 'Configure' } else { 'Apply' }
        $journal = New-ConvtFinishJournal -Root $root -PreviousRoot $previous -Id $Id -Operation $operation
        Write-ConvtFinishJournal -Record $journal -Root $root -Id $Id
        if ($Action -eq 'Prepare') { return }
    }
    if (-not $journal) { throw 'The installer integration snapshot is missing.' }
    if ($Action -eq 'Remove') {
        $journal.Operation = 'Remove'
        Write-ConvtFinishJournal -Record $journal -Root $root -Id $Id
        $Menu=$false; $Desktop=$false
    }
    try {
        Apply-ConvtFinishOptions -Journal $journal -Menu $Menu -Desktop $Desktop -Removing:($Action -eq 'Remove')
        if (-not $HoldTransaction) { Remove-ConvtFinishJournal -Root $root -Id $Id }
    } catch {
        $failure = $_.Exception.Message
        # MSI rollback runs after it restores the previous payload. A final-page
        # configuration has no pending file transaction and compensates now.
        if (-not $HoldTransaction) {
            try { Undo-ConvtFinishJournal -Journal $journal; Remove-ConvtFinishJournal -Root $root -Id $Id }
            catch { throw "Installer options failed: $failure Compensation also failed: $($_.Exception.Message)" }
        }
        throw "Installer options failed: $failure"
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    if ($ResultPath) {
        $result = [IO.Path]::GetFullPath($ResultPath)
        if ([IO.Path]::GetDirectoryName($result) -ine $PSScriptRoot -or
            [IO.Path]::GetFileName($result) -notmatch '\Aresult-[a-f0-9]{32}\.log\z' -or
            -not [IO.File]::Exists($result) -or (Get-Item -LiteralPath $result).Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw 'The result log must be the native dispatcher-owned regular file in the helper cache.'
        }
    }
    try {
        $expectedCore = Join-Path $PSScriptRoot 'modern-menu.ps1'
        if ($CoreModernScript -and [IO.Path]::GetFullPath($CoreModernScript) -ine [IO.Path]::GetFullPath($expectedCore)) {
            throw 'Only the adjacent embedded modern menu helper may be loaded.'
        }
        Import-ConvtMenuFunctions -Path $expectedCore
        Invoke-ConvtFinishOptions -Action $Action -Root $InstallationRoot -PreviousRoot $PreviousInstallationRoot -Menu:($ContextMenu -eq '1') -Desktop:($DesktopShortcut -eq '1') -Id $TransactionId -HoldTransaction:$HoldTransaction
        if ($ResultPath) { [IO.File]::WriteAllText($ResultPath, 'Installer options completed.', [Text.UTF8Encoding]::new($false)) }
    } catch {
        if ($ResultPath) { [IO.File]::WriteAllText($ResultPath, $_.Exception.Message, [Text.UTF8Encoding]::new($false)) }
        throw
    }
}
