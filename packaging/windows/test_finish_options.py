"""Exercise installer options with mocked OS leaves, never registry/Appx/GUI writes."""

import json
import os
import shutil
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
POWERSHELL = os.environ.get("CONVT_TEST_POWERSHELL") or shutil.which("pwsh") or shutil.which("powershell")
TX = "b3fbe30a-4ed7-4a24-86b7-89b11211ea08"
CONFIGURE_TX = "ee2aeaa8-e5a8-4f90-b8cb-e18da4ca1d75"
OLD = "Convt.Desktop_0.3.4.0_x64__fixture"
NEW = "Convt.Desktop_0.3.5.0_x64__fixture"
PROBE = r"""
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
. $env:CONVT_TEST_FINISH_SOURCE
Import-ConvtMenuFunctions -Path $env:CONVT_TEST_MENU_SOURCE
$case=$env:CONVT_TEST_FINISH_CASE | ConvertFrom-Json
$script:Events=[Collections.Generic.List[string]]::new()
$script:Packages=@($case.packages)
$script:FailPreferenceWrite=$case.preferenceFail
$script:Legacy=if ($case.legacy) {New-ConvtLegacyRecord -Root $case.beforeRoot} else {$null}
$script:Backups=@{}
if ($case.legacyBackup) {$script:Backups[$case.beforeRoot]=New-ConvtLegacyRecord -Root $case.beforeRoot}
$script:PreferenceValues=@{}
$script:PreferencesExist=$case.preferences
if ($case.preferences) {
 foreach ($prop in ([pscustomobject]@{Name='Convt.Desktop';Publisher='CN=Convt Local';InstallationRoot=$case.beforeRoot
 ContextMenu='1';DesktopShortcut=([int]$case.shortcut).ToString()}).PSObject.Properties) {
  $script:PreferenceValues[$prop.Name]=$prop.Value
 }
}
if ($case.badPreferences) {$script:PreferenceValues=@{Unexpected='foreign'};$script:PreferencesExist=$true}
if ($case.shortcut -or $case.foreignShortcut) {
 $owner=if ($case.foreignShortcut) {'C:\foreign'} else {$case.beforeRoot}
 [IO.File]::WriteAllText($case.shortcutPath,('shortcut:'+$owner))
}
function Open-ConvtInstallerPreferencesKey {
 if (-not $script:PreferencesExist) {return $null}
 $key=[pscustomobject]@{}
 $key | Add-Member ScriptMethod GetValueNames {@($script:PreferenceValues.Keys)}
 $key | Add-Member ScriptMethod GetValueKind {param($Name) [Microsoft.Win32.RegistryValueKind]::String}
 $key | Add-Member ScriptMethod GetValue {param($Name) $script:PreferenceValues[$Name]}
 $key | Add-Member ScriptMethod Dispose {}
 $key
}
function Set-ConvtInstallerPreferences {
 param($Record,$Root)
 Assert-ConvtInstallerPreferences -Record $Record -Root $Root
 $script:Events.Add('save-preferences')
 $script:PreferencesExist=$true;$script:PreferenceValues=@{}
 foreach ($prop in $Record.PSObject.Properties) {$script:PreferenceValues[$prop.Name]=$prop.Value}
 if ($script:FailPreferenceWrite) {$script:FailPreferenceWrite=$false;throw 'preference write failed after mutation'}
}
function Remove-ConvtInstallerPreferences {
 param($Root)
 $record=Get-ConvtInstallerPreferences -Root $Root
 if ($record) {$script:Events.Add('remove-preferences');$script:PreferenceValues=@{}}
 # Like the real implementation, preserve the empty key/foreign subkeys.
}
function Get-ConvtDesktopShortcutPath {$case.shortcutPath}
function Get-ConvtOwnedDesktopShortcut {
 param($Root,[switch]$RequireOwned)
 if (-not [IO.File]::Exists($case.shortcutPath)) {return $null}
 if ([IO.File]::ReadAllText($case.shortcutPath) -ine ('shortcut:'+$Root)) {
  if ($RequireOwned) {throw 'The existing convt.lnk points to another application; it was preserved.'}
  return $null
 }
 [pscustomobject]@{Root=$Root;Bytes=[Convert]::ToBase64String([IO.File]::ReadAllBytes($case.shortcutPath))}
}
function Set-ConvtOwnedDesktopShortcut {
 param($Root)
 [void](Get-ConvtOwnedDesktopShortcut -Root $Root -RequireOwned)
 $script:Events.Add('create-shortcut')
 [IO.File]::WriteAllText($case.shortcutPath,('shortcut:'+$Root))
}
function Remove-ConvtOwnedDesktopShortcut {
 param($Root)
 if (Get-ConvtOwnedDesktopShortcut -Root $Root) {$script:Events.Add('remove-shortcut');[IO.File]::Delete($case.shortcutPath)}
}
function Get-ConvtFinishJournalPath {param([guid]$Id) Join-Path $case.journalRoot ($Id.ToString('D')+'.finish.json')}
function Get-ConvtWindowsBuild {$case.build}
function Test-ConvtDeveloperMode {$case.developer}
function Get-ConvtInstalledPackages {$script:Packages}
function Get-ConvtPackageExternalRoot {
 param($PackageFullName)
 ($script:Packages | Where-Object PackageFullName -ceq $PackageFullName | Select-Object -First 1).ExternalRoot
}
function Register-ConvtIdentity {
 param($ManifestPath,$Root,$SignedPath)
 $script:Events.Add('register:'+ $Root)
 if ($case.registerFail -and $Root -ieq $case.root) {throw 'registration failed'}
 $document=Read-ConvtMenuManifest -Path $ManifestPath
 $version=$document.GetElementsByTagName('Identity').Item(0).GetAttribute('Version')
 $script:Packages += [pscustomobject]@{Name='Convt.Desktop';Publisher='CN=Convt Local';Version=$version
  PackageFullName=('Convt.Desktop_'+$version+'_x64__fixture');InstallLocation=(Join-Path $Root 'identity');ExternalRoot=$Root}
}
function Unregister-ConvtIdentity {
 param($PackageFullName)
 $script:Events.Add('unregister:'+ $PackageFullName)
 $script:Packages=@($script:Packages | Where-Object PackageFullName -cne $PackageFullName)
}
function Test-ConvtModernComActivation {
 $script:Events.Add('activate-modern')
 if ($case.activationFail) {throw 'COM activation failed (HRESULT 0x80070005)'}
}
function Get-ConvtLegacyMenu {
 param($Root)
 if ($script:Legacy) {Assert-ConvtLegacyRecord -Record $script:Legacy -Root $Root}
 $script:Legacy
}
function Restore-ConvtLegacyMenu {
 param($Record,$Root)
 Assert-ConvtLegacyRecord -Record $Record -Root $Root
 $script:Events.Add('restore-classic');$script:Legacy=$Record
}
function Remove-ConvtLegacyMenu {
 param($Record,$Root)
 Assert-ConvtLegacyRecord -Record $Record -Root $Root
 $script:Events.Add('remove-classic');$script:Legacy=$null
}
function Read-ConvtLegacyBackup {
 param($Root)
 $backup=$script:Backups[$Root]
 if ($backup) {Assert-ConvtLegacyRecord -Record $backup -Root $Root}
 $backup
}
function Save-ConvtLegacyBackup {param($Record,$Root) $script:Backups[$Root]=$Record}
function Remove-ConvtLegacyBackup {param($Root) [void]$script:Backups.Remove($Root)}
$results=@()
foreach ($step in $case.steps) {
 if ($step.restoreOld) {[IO.Directory]::Move($case.beforeRoot+'.missing',$case.beforeRoot)}
 if ($step.hideOld) {[IO.Directory]::Move($case.beforeRoot,$case.beforeRoot+'.missing')}
 if ($step.tamperBackup) {
  $backup=Get-ConvtFinishBackupRoot -Id ([guid]$case.id)
  [IO.File]::WriteAllText((Join-Path $backup 'convt_shell.dll'),'changed snapshot')
 }
 if ($step.foreignOldFile) {
  [void][IO.Directory]::CreateDirectory($case.beforeRoot)
  [IO.File]::WriteAllText((Join-Path $case.beforeRoot 'convt.exe'),'foreign replacement')
 }
 if ($step.extraBackupFile) {
  [IO.File]::WriteAllText((Join-Path (Get-ConvtFinishBackupRoot -Id ([guid]$case.id)) 'foreign.txt'),'preserve me')
 }
 if ($step.changePreferences) {$script:PreferenceValues['ContextMenu']='0'}
 if ($null -ne $step.activationFail) {$case.activationFail=$step.activationFail}
 try {
  Invoke-ConvtFinishOptions -Action $step.action -Root $case.root -PreviousRoot $case.previousRoot -Id ([guid]$step.id) -Menu:($step.menu) -Desktop:($step.desktop) -HoldTransaction:($step.hold)
  $results += @{ok=$true}
 } catch {$results += @{ok=$false;error=$_.Exception.Message}}
}
$prefs=if ($script:PreferenceValues.Count) {[pscustomobject]$script:PreferenceValues} else {$null}
ConvertTo-Json -InputObject @{results=$results;events=@($script:Events);legacy=$script:Legacy
 packages=@($script:Packages | ForEach-Object {$_.PackageFullName});preferences=$prefs
 legacyBackupRoots=@($script:Backups.Keys)
 shortcut=if ([IO.File]::Exists($case.shortcutPath)) {[IO.File]::ReadAllText($case.shortcutPath)} else {$null}
 backup=[IO.Directory]::Exists((Get-ConvtFinishBackupRoot -Id ([guid]$case.id)))
 backupFileCount=@(Get-ConvtFinishPackageFiles | Where-Object {[IO.File]::Exists((Join-Path (Get-ConvtFinishBackupRoot -Id ([guid]$case.id)) $_))}).Count
 separateMarkers=@($case.steps | ForEach-Object {$_.id} | Where-Object {$_ -ne $case.id} | Select-Object -Unique | ForEach-Object {
  [pscustomobject]@{Id=$_;Exists=[IO.File]::Exists((Join-Path $case.journalRoot ($_+'.finish.json')))}
 })
 oldCli=if ([IO.File]::Exists((Join-Path $case.beforeRoot 'convt.exe'))) {[Convert]::ToBase64String([IO.File]::ReadAllBytes((Join-Path $case.beforeRoot 'convt.exe')))} else {$null}
 marker=[IO.File]::Exists((Join-Path $case.journalRoot ($case.id+'.finish.json')))} -Depth 9 -Compress
"""


def runtime(root, version):
    assets = root / "identity" / "Assets"
    assets.mkdir(parents=True)
    manifest = (HERE / "AppxManifest.xml").read_text(encoding="utf-8").replace("0.3.2.0", version)
    (root / "identity" / "AppxManifest.xml").write_text(manifest, encoding="utf-8")
    for name in ("StoreLogo.png", "Square150x150Logo.png", "Square44x44Logo.png"):
        (assets / name).write_bytes(b"\x89PNG\r\n\x1a\n")
    for name in ("convt.exe", "convt-app.exe", "convt_shell.dll"):
        pe = bytearray(160)
        pe[:2] = b"MZ"
        struct.pack_into("<I", pe, 0x3C, 64)
        pe[64:68] = b"PE\0\0"
        struct.pack_into("<H", pe, 68, 0x8664)
        struct.pack_into("<H", pe, 86, 0x2000 if name.endswith(".dll") else 0)
        (root / name).write_bytes(pe)


@unittest.skipUnless(os.name == "nt" and POWERSHELL, "requires Windows PowerShell for mocked lifecycle")
class FinishOptions(unittest.TestCase):
    def run_case(self, *, upgrade=False, steps=None, **changes):
        scratch = HERE.parents[1] / "tmp"
        scratch.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="finish-options-test-", dir=scratch) as folder:
            root = Path(folder) / "Programs with spaces & %" / "convt-new"
            before = Path(folder) / "convt-old" if upgrade else root
            runtime(root, "0.3.5.0")
            if upgrade:
                runtime(before, "0.3.4.0")
            case = dict(root=str(root), beforeRoot=str(before), previousRoot=str(before) if upgrade else "",
                        journalRoot=str(Path(folder) / "journal"), shortcutPath=str(Path(folder) / "convt.shortcut-fixture"), id=TX,
                        build=26100, developer=True, legacy=True, legacyBackup=False, preferences=False, shortcut=False,
                        foreignShortcut=False, badPreferences=False, activationFail=False, registerFail=False, preferenceFail=False,
                        packages=[], steps=[])
            case.update(changes)
            settings = Path(folder) / "settings.toml"
            presets = Path(folder) / "presets.json"
            settings.write_bytes(b"# user preference\n[windows_menu]\nenabled = false\ntargets = ['webp']\n")
            presets.write_bytes(b'{"my-preset":{"quality":91}}')
            settings_before, presets_before = settings.read_bytes(), presets.read_bytes()
            if upgrade:
                case["packages"] = [dict(Name="Convt.Desktop", Publisher="CN=Convt Local", Version="0.3.4.0",
                                         InstallLocation=str(before / "identity"), ExternalRoot=str(before), PackageFullName=OLD)]
            case["steps"] = [{"menu": True, "desktop": False, "hold": False, "hideOld": False, "restoreOld": False,
                              "activationFail": None, "tamperBackup": False, "foreignOldFile": False,
                              "extraBackupFile": False, "changePreferences": False,
                              "id": TX,
                              **step} for step in steps or [dict(action="Configure")]]
            result = subprocess.run([POWERSHELL, "-NoProfile", "-NonInteractive", "-Command", PROBE],
                                    env={**os.environ, "CONVT_TEST_FINISH_SOURCE": str(HERE / "finish-options.ps1"),
                                         "CONVT_TEST_MENU_SOURCE": str(HERE / "modern-menu.ps1"),
                                         "CONVT_TEST_FINISH_CASE": json.dumps(case)},
                                    capture_output=True, text=True, encoding="utf-8", timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(settings.read_bytes(), settings_before)
            self.assertEqual(presets.read_bytes(), presets_before)
            value = json.loads(result.stdout)
            value["root"] = str(root)
            value["oldRoot"] = str(before)
            return value

    def test_four_finish_choices_are_effective(self):
        for menu in (False, True):
            for desktop in (False, True):
                with self.subTest(menu=menu, desktop=desktop):
                    result = self.run_case(steps=[dict(action="Configure", menu=menu, desktop=desktop)])
                    self.assertTrue(result["results"][0]["ok"], result)
                    self.assertEqual(result["packages"], [NEW] if menu else [])
                    self.assertEqual(bool(result["shortcut"]), desktop)
                    self.assertEqual(result["preferences"]["ContextMenu"], str(int(menu)))
                    self.assertEqual(result["preferences"]["DesktopShortcut"], str(int(desktop)))
                    self.assertFalse(result["marker"])

    def test_upgrade_transfers_package_and_configure_uses_current_root(self):
        result = self.run_case(upgrade=True, preferences=True, steps=[
            dict(action="Prepare"), dict(action="Apply", hold=True, hideOld=True), dict(action="Commit"),
            dict(action="Configure", menu=False),
        ])
        self.assertTrue(all(row["ok"] for row in result["results"]), result)
        self.assertEqual(result["packages"], [])
        self.assertEqual(result["preferences"]["InstallationRoot"], result["root"])

    def test_failed_upgrade_restores_old_package_after_msi_restores_files(self):
        result = self.run_case(upgrade=True, preferences=True, shortcut=True, steps=[
            dict(action="Prepare"), dict(action="Apply", desktop=True, hold=True, activationFail=True, hideOld=True),
            dict(action="Rollback", restoreOld=True),
        ])
        self.assertTrue(result["results"][0]["ok"], result)
        self.assertFalse(result["results"][1]["ok"], result)
        self.assertTrue(result["results"][2]["ok"], result)
        self.assertEqual(result["packages"], [OLD])
        self.assertEqual(result["shortcut"], "shortcut:" + result["oldRoot"])
        self.assertEqual(result["preferences"]["InstallationRoot"], result["oldRoot"])
        self.assertFalse(result["marker"])

    def test_upgrade_rollback_restores_old_legacy_backup_and_removes_only_new_one(self):
        result = self.run_case(upgrade=True, legacy=False, legacyBackup=True, steps=[
            dict(action="Prepare"), dict(action="Apply", hold=True, hideOld=True), dict(action="Rollback"),
        ])
        self.assertTrue(all(row["ok"] for row in result["results"]), result)
        self.assertEqual(result["packages"], [OLD])
        self.assertEqual(result["legacyBackupRoots"], [result["oldRoot"]])

    def test_early_upgrade_rollback_restores_package_before_msi_old_files(self):
        result = self.run_case(upgrade=True, preferences=True, steps=[
            dict(action="Prepare"), dict(action="Apply", hold=True, activationFail=True, hideOld=True),
            dict(action="Rollback"),
        ])
        self.assertTrue(result["results"][0]["ok"], result)
        self.assertFalse(result["results"][1]["ok"], result)
        self.assertTrue(result["results"][2]["ok"], result)
        self.assertEqual(result["packages"], [OLD])
        self.assertFalse(result["marker"])
        self.assertFalse(result["backup"])

    def test_failure_between_old_removal_and_apply_recovers_before_files(self):
        result = self.run_case(upgrade=True, steps=[dict(action="Prepare"), dict(action="Rollback", hideOld=True)])
        self.assertTrue(all(row["ok"] for row in result["results"]), result)
        self.assertEqual(result["packages"], [OLD])
        self.assertIsNotNone(result["oldCli"])
        self.assertFalse(result["marker"])
        self.assertFalse(result["backup"])

    def test_retry_same_product_code_recovers_only_unattempted_identical_baseline(self):
        result = self.run_case(upgrade=True, preferences=True, steps=[dict(action="Prepare"), dict(action="Prepare"),
                                     dict(action="Apply", hold=True), dict(action="Commit")])
        self.assertTrue(all(row["ok"] for row in result["results"]), result)
        self.assertEqual(result["packages"], [NEW])
        self.assertFalse(result["marker"])
        self.assertFalse(result["backup"])

    def test_unattempted_retry_preserves_snapshot_when_baseline_changes(self):
        result = self.run_case(upgrade=True, preferences=True, steps=[dict(action="Prepare"),
                                                                  dict(action="Prepare", changePreferences=True)])
        self.assertTrue(result["results"][0]["ok"], result)
        self.assertFalse(result["results"][1]["ok"], result)
        self.assertTrue(result["marker"])
        self.assertEqual(result["backupFileCount"], 7)
        self.assertEqual(result["packages"], [OLD])

    def test_commit_foreign_contents_preflight_preserves_all_backup_files(self):
        result = self.run_case(upgrade=True, steps=[dict(action="Prepare"), dict(action="Apply", hold=True),
                                                  dict(action="Commit", extraBackupFile=True)])
        self.assertTrue(result["results"][1]["ok"], result)
        self.assertFalse(result["results"][2]["ok"], result)
        self.assertTrue(result["marker"])
        self.assertEqual(result["backupFileCount"], 7)
        self.assertEqual(result["packages"], [NEW])

    def test_configure_rollback_isolated_from_committed_lifecycle_journal(self):
        result = self.run_case(upgrade=True, steps=[dict(action="Prepare"), dict(action="Apply", hold=True),
            dict(action="Commit", extraBackupFile=True),
            dict(action="Configure", id=CONFIGURE_TX, hold=True, activationFail=True),
            dict(action="Rollback", id=CONFIGURE_TX),
        ])
        self.assertTrue(result["results"][1]["ok"], result)
        self.assertFalse(result["results"][2]["ok"], result)
        self.assertFalse(result["results"][3]["ok"], result)
        self.assertTrue(result["results"][4]["ok"], result)
        self.assertEqual(result["packages"], [NEW])
        self.assertTrue(result["marker"], "preserve the diagnostic lifecycle journal")
        self.assertEqual(result["backupFileCount"], 7)
        self.assertEqual(result["separateMarkers"], [{"Id": CONFIGURE_TX, "Exists": False}])

    def test_snapshot_tamper_or_divergent_old_file_fails_closed(self):
        for guard in ("tamperBackup", "foreignOldFile"):
            with self.subTest(guard=guard):
                result = self.run_case(upgrade=True, steps=[
                    dict(action="Prepare"), dict(action="Apply", hold=True, activationFail=True, hideOld=True),
                    dict(action="Rollback", **{guard: True}),
                ])
                self.assertFalse(result["results"][2]["ok"], result)
                self.assertTrue(result["marker"])
                self.assertTrue(result["backup"])
                self.assertEqual(result["packages"], [])
                if guard == "foreignOldFile":
                    import base64
                    self.assertEqual(base64.b64decode(result["oldCli"]), b"foreign replacement")

    def test_old_shortcut_transfers_or_is_removed_for_both_checkbox_values(self):
        for desktop in (False, True):
            with self.subTest(desktop=desktop):
                result = self.run_case(upgrade=True, shortcut=True, steps=[
                    dict(action="Prepare"), dict(action="Apply", desktop=desktop, hold=True), dict(action="Commit"),
                ])
                self.assertTrue(all(row["ok"] for row in result["results"]), result)
                self.assertEqual(result["shortcut"], "shortcut:" + result["root"] if desktop else None)

    def test_classic_fallback_removes_old_package(self):
        for change in (dict(build=19045), dict(developer=False)):
            result = self.run_case(upgrade=True, steps=[dict(action="Prepare"), dict(action="Apply"), dict(action="Commit")], **change)
            self.assertTrue(all(row["ok"] for row in result["results"]), result)
            self.assertEqual(result["packages"], [])
            self.assertEqual(result["legacy"]["InstallationRoot"], result["root"])
            self.assertNotIn("activate-modern", result["events"])

    def test_foreign_desktop_shortcut_is_preserved(self):
        result = self.run_case(foreignShortcut=True, steps=[dict(action="Configure", desktop=True)])
        self.assertFalse(result["results"][0]["ok"], result)
        self.assertEqual(result["shortcut"], "shortcut:C:\\foreign")
        self.assertEqual(result["events"], [])
        self.assertFalse(result["marker"])

    def test_uninstall_empty_preference_key_allows_reinstall(self):
        result = self.run_case(preferences=True, shortcut=True, steps=[
            dict(action="Prepare"), dict(action="Remove", hold=True), dict(action="Commit"),
            dict(action="Prepare"), dict(action="Apply", hold=True), dict(action="Commit"),
        ])
        self.assertTrue(all(row["ok"] for row in result["results"]), result)
        self.assertEqual(result["packages"], [NEW])
        self.assertEqual(result["preferences"]["ContextMenu"], "1")

    def test_configure_failure_compensates_and_retry_succeeds(self):
        result = self.run_case(steps=[dict(action="Configure", activationFail=True),
                                      dict(action="Configure", activationFail=False)])
        self.assertFalse(result["results"][0]["ok"], result)
        self.assertTrue(result["results"][1]["ok"], result)
        self.assertEqual(result["packages"], [NEW])
        self.assertFalse(result["marker"])

    def test_configure_failure_after_preferences_write_allows_retry_with_empty_key(self):
        result = self.run_case(preferenceFail=True, steps=[dict(action="Configure"), dict(action="Configure")])
        self.assertFalse(result["results"][0]["ok"], result)
        self.assertTrue(result["results"][1]["ok"], result)
        self.assertEqual(result["packages"], [NEW])
        self.assertFalse(result["marker"])

    def test_uninstall_rollback_restores_package_shortcut_and_preferences(self):
        result = self.run_case(steps=[dict(action="Configure", desktop=True), dict(action="Prepare"),
                                      dict(action="Remove", hold=True), dict(action="Rollback")])
        self.assertTrue(all(row["ok"] for row in result["results"]), result)
        self.assertEqual(result["packages"], [NEW])
        self.assertEqual(result["shortcut"], "shortcut:" + result["root"])
        self.assertEqual(result["preferences"]["ContextMenu"], "1")
        self.assertEqual(result["preferences"]["DesktopShortcut"], "1")
        self.assertFalse(result["marker"])

    def test_nonempty_malformed_preferences_fail_before_mutation(self):
        result = self.run_case(badPreferences=True, steps=[dict(action="Prepare")])
        self.assertFalse(result["results"][0]["ok"], result)
        self.assertEqual(result["events"], [])
        self.assertFalse(result["marker"])


if __name__ == "__main__":
    unittest.main()
