"""Check modern-menu authoring and mocked lifecycle; never deploy an Appx package."""

import json
import os
import shutil
import struct
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

HERE = Path(__file__).resolve().parent
POWERSHELL = shutil.which("pwsh") or shutil.which("powershell")
FULL = "Convt.Desktop_0.3.2.0_x64__fixture"
TX = "b3fbe30a-4ed7-4a24-86b7-89b11211ea08"
PROBE = r"""
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false)
$tokens=$null; $errors=$null
$ast=[System.Management.Automation.Language.Parser]::ParseFile($env:CONVT_TEST_MENU_SOURCE,[ref]$tokens,[ref]$errors)
if ($errors) { throw 'Modern menu helper contains syntax errors' }
# OS leaves and top-level code are never imported.
$allowed=@('Get-ConvtMenuIdentity','Assert-ConvtIdentityVersion','Assert-ConvtMenuManifest',
 'Read-ConvtMenuManifest','Resolve-ConvtInstallationRoot','Assert-ConvtInstalledBinary',
 'Assert-ConvtInstallation','Select-ConvtIdentityPackages','Assert-ConvtRegisteredPackage',
 'Assert-ConvtPackageRoot','Assert-ConvtLegacyRecord','Read-ConvtLegacyBackup',
 'Save-ConvtLegacyBackup','Remove-ConvtLegacyBackup','Assert-ConvtMenuTransaction',
 'Read-ConvtMenuTransaction','Write-ConvtMenuTransaction','Remove-ConvtMenuTransaction',
 'Undo-ConvtMenuTransaction','Invoke-ConvtModernMenu','Register-ConvtIdentity','Unregister-ConvtIdentity')
foreach ($name in $allowed) {
 $fn=$ast.Find({param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name},$true)
 if (-not $fn) { throw "Missing modern-menu function: $name" }
 . ([scriptblock]::Create($fn.Extent.Text))
}
$case=$env:CONVT_TEST_MENU_CASE | ConvertFrom-Json
$script:Events=[System.Collections.Generic.List[string]]::new()
$script:Packages=@($case.packages)
$script:Legacy=if ($case.legacy) {
 [pscustomobject]@{Name='Convt.Desktop';Publisher='CN=Convt Local';InstallationRoot=$case.root
 Values=[pscustomobject]@{ExplorerCommandHandler='{710FB9A8-C47E-4B39-9CFA-E273AB1B78F8}'
 MUIVerb='Convert with convt';MultiSelectModel='Player';Icon=('"'+(Join-Path $case.root 'convt-app.exe')+'",0')}}
} else {$null}
if ($case.badLegacy) {$script:Legacy.Values.ExplorerCommandHandler='{00000000-0000-0000-0000-000000000001}'}
function Get-ConvtWindowsBuild {$case.build}
function Test-ConvtDeveloperMode {$case.developer}
function Get-ConvtInstalledPackages {$script:Packages}
function Test-ConvtModernComActivation {
 $script:Events.Add('activate-modern')
 if ($case.activationFail) {throw 'COM activation failed (HRESULT 0x80070005)'}
}
function Get-ConvtTransactionPath {param([guid]$Id) Join-Path $case.markerRoot ($Id.ToString('D')+'.json')}
function Get-ConvtPackageExternalRoot {
 param($PackageFullName)
 if ($case.externalError) {throw 'external location query failed'}
 if ($case.external) {$case.external} else {$case.root}
}
function Add-AppxPackage {
 param($Path,$ExternalLocation,[switch]$Register,$ErrorAction)
 $script:Events.Add('register')
 if (-not $Register -or $ExternalLocation -cne $case.root -or $Path -cne (Join-Path $case.root 'identity\AppxManifest.xml')) {
  throw 'Appx command did not use the validated per-user loose manifest and external root'
 }
 if ($case.registerFail) {throw 'registration failed'}
 $doc=Read-ConvtMenuManifest -Path $Path
 $version=$doc.GetElementsByTagName('Identity').Item(0).GetAttribute('Version')
 $script:Packages += [pscustomobject]@{Name='Convt.Desktop';Publisher='CN=Convt Local';Version=$version
 InstallLocation=(Split-Path -Parent $Path);PackageFullName=('Convt.Desktop_'+$version+'_x64__fixture')}
}
function Remove-AppxPackage {
 param($Package,$ErrorAction)
 $script:Events.Add('unregister:'+$Package)
 $script:Packages=@($script:Packages | Where-Object PackageFullName -ne $Package)
}
function Get-ConvtLegacyMenu {
 param($Root)
 if ($script:Legacy) {Assert-ConvtLegacyRecord -Record $script:Legacy -Root $Root}
 $script:Legacy
}
function Remove-ConvtLegacyMenu {
 param($Record,$Root)
 $script:Events.Add('remove-classic'); $script:Legacy=$null
 if ($case.removeLegacyFail) {throw 'legacy removal failed after mutation'}
}
function Restore-ConvtLegacyMenu {
 param($Record,$Root)
 Assert-ConvtLegacyRecord -Record $Record -Root $Root
 $script:Events.Add('restore-classic')
 if ($case.restoreLegacyFail) {throw 'legacy restoration failed'}
 $script:Legacy=$Record
}
$results=@()
foreach ($step in $case.steps) {
 if ($step.restoreFiles) {[System.IO.Directory]::Move($case.root+'.missing',$case.root)}
 try {
  $args=@{Action=$step.action;InstallationRoot=$case.root;RestoreClassic=$step.restore
   SignedPackagePath=$step.signed;TransactionId=[guid]$step.transaction}
  $result=Invoke-ConvtModernMenu @args
  $results += @{ok=$true;result=$result}
 } catch {$results += @{ok=$false;error=$_.Exception.Message}}
 if ($step.hideFiles) {[System.IO.Directory]::Move($case.root,$case.root+'.missing')}
}
ConvertTo-Json -InputObject @{results=$results;events=@($script:Events);legacy=[bool]$script:Legacy
 packages=@($script:Packages | ForEach-Object PackageFullName)
 marker=[System.IO.File]::Exists((Join-Path $case.markerRoot ($case.markerId+'.json')))} -Depth 8 -Compress
"""

NATIVE_API_PROBE = r"""
$ErrorActionPreference = 'Stop'
$tokens = $null; $errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    $env:CONVT_TEST_MENU_SOURCE, [ref]$tokens, [ref]$errors)
if ($errors) { throw 'Modern menu helper has syntax errors' }
$function = $ast.Find({ param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -eq 'Get-ConvtPackageExternalRoot'
}, $true)
. ([scriptblock]::Create($function.Extent.Text))
# This deliberately absent package exercises the real DLL export and native
# call without registering, removing or requiring any actual package.
try {
    Get-ConvtPackageExternalRoot -PackageFullName 'Convt.NativeApiProbe_0.0.0.0_x64__8wekyb3d8bbwe'
    throw 'The probe package unexpectedly exists'
} catch {
    if ($_.Exception.Message -notmatch 'Cannot query convt package external location \(Win32 [0-9]+\)') { throw }
    Write-Output 'Native package-path API called successfully'
}
"""

NATIVE_COM_DEFINITION_PROBE = r"""
$ErrorActionPreference = 'Stop'
$tokens = $null; $errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    $env:CONVT_TEST_MENU_SOURCE, [ref]$tokens, [ref]$errors)
if ($errors) { throw 'Modern menu helper has syntax errors' }
$function = $ast.Find({ param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -eq 'Invoke-ConvtModernComActivationProbe'
}, $true)
if (-not $function) { throw 'Missing COM activation guard' }
$definition = $function.Find({ param($node)
    $node -is [System.Management.Automation.Language.StringConstantExpressionAst] -and
    $node.Value.Contains('public static class ComActivation')
}, $true)
if (-not $definition) { throw 'Missing native COM guard definition' }
# Compile and inspect signatures only: never call Test or any native COM API.
Add-Type -TypeDefinition $definition.Value
$type = 'Convt.ModernMenu.ComActivation' -as [type]
if (-not $type.GetMethod('Test')) { throw 'Missing public guard entry point' }
$flags = [System.Reflection.BindingFlags]::NonPublic -bor [System.Reflection.BindingFlags]::Static
foreach ($name in @('CoInitializeEx', 'CoCreateInstance', 'CoUninitialize')) {
    $method = $type.GetMethod($name, $flags)
    if (-not $method) { throw "Missing native COM signature: $name" }
    $import = @($method.GetCustomAttributes([System.Runtime.InteropServices.DllImportAttribute], $false))
    if ($import.Count -ne 1 -or $import[0].Value -cne 'ole32.dll' -or -not $import[0].ExactSpelling) {
        throw "Unexpected native COM import: $name"
    }
}
Write-Output 'COM activation guard compiled without activation'
"""

CHILD_CONTROLLER_PROBE = r"""
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$tokens=$null; $errors=$null
$ast=[System.Management.Automation.Language.Parser]::ParseFile($env:CONVT_TEST_MENU_SOURCE,[ref]$tokens,[ref]$errors)
if ($errors) {throw 'Helper syntax errors'}
foreach ($name in @('Get-ConvtMenuIdentity','Test-ConvtModernComActivation','Invoke-ConvtMenuHelper')) {
    $function=$ast.Find({param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
    },$true)
    if (-not $function) {throw "Missing guard function: $name"}
    . ([scriptblock]::Create($function.Extent.Text))
}
$case=$env:CONVT_TEST_MENU_CASE | ConvertFrom-Json
$script:ConvtMenuSourcePath=$env:CONVT_TEST_MENU_SOURCE
$script:Events=[System.Collections.Generic.List[string]]::new()
$script:Arguments=''
function Start-ConvtModernComActivationProcess {
    param([System.Diagnostics.ProcessStartInfo]$StartInfo)
    if ($StartInfo.UseShellExecute -or -not $StartInfo.CreateNoWindow -or
        -not $StartInfo.RedirectStandardOutput -or -not $StartInfo.RedirectStandardError -or
        $StartInfo.FileName -notlike '*\System32\WindowsPowerShell\v1.0\powershell.exe') {
        throw 'Child process was not configured safely'
    }
    $script:Arguments=$StartInfo.Arguments
    $script:Events.Add('start')
    $stdout=[pscustomobject]@{Text=$case.stdout}
    $stderr=[pscustomobject]@{Text=$case.stderr}
    foreach ($stream in @($stdout,$stderr)) {
        $stream | Add-Member -MemberType ScriptMethod -Name ReadToEndAsync -Value {
            [pscustomobject]@{Result=$this.Text}
        }
    }
    $process=[pscustomobject]@{HasExited=$false;ExitCode=$case.exitCode;StandardOutput=$stdout;StandardError=$stderr}
    $process | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value {
        param([Nullable[int]]$Milliseconds)
        if ($null -eq $Milliseconds) {
            $script:Events.Add('reap');$this.HasExited=$true;return
        }
        $script:Events.Add('wait:'+$Milliseconds)
        $this.HasExited=-not $case.timeout
        return $this.HasExited
    }
    $process | Add-Member -MemberType ScriptMethod -Name Kill -Value {$script:Events.Add('kill')}
    $process | Add-Member -MemberType ScriptMethod -Name Dispose -Value {$script:Events.Add('dispose')}
    $process
}
function Invoke-ConvtModernComActivationProbe {$script:Events.Add('native-probe')}
function Invoke-ConvtModernMenu {$script:Events.Add('menu-lifecycle')}
try {
    if ($case.entry) {
        [void](Invoke-ConvtMenuHelper -Action $case.action -ClassId ([guid]$case.classId))
    } else {Test-ConvtModernComActivation}
    $result=@{ok=$true}
} catch {$result=@{ok=$false;error=$_.Exception.Message}}
ConvertTo-Json -InputObject @{result=$result;events=@($script:Events);arguments=$script:Arguments} -Depth 4 -Compress
"""


@unittest.skipUnless(os.name == "nt" and POWERSHELL, "requires Windows PowerShell")
class NativePackagePathApi(unittest.TestCase):
    def test_documented_package_path_export_is_callable_readonly(self):
        result = subprocess.run(
            [POWERSHELL, "-NoProfile", "-NonInteractive", "-Command", NATIVE_API_PROBE],
            env={**os.environ, "CONVT_TEST_MENU_SOURCE": str(HERE / "modern-menu.ps1")},
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Native package-path API called successfully", result.stdout)

    def test_com_activation_guard_compiles_without_calling_com(self):
        result = subprocess.run(
            [POWERSHELL, "-NoProfile", "-NonInteractive", "-Command", NATIVE_COM_DEFINITION_PROBE],
            env={**os.environ, "CONVT_TEST_MENU_SOURCE": str(HERE / "modern-menu.ps1")},
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("COM activation guard compiled without activation", result.stdout)


@unittest.skipUnless(os.name == "nt" and POWERSHELL, "requires Windows PowerShell for mocked child")
class ModernMenuChildGuard(unittest.TestCase):
    def run_case(self, **changes):
        case = {"timeout": False, "exitCode": 0, "stdout": "", "stderr": "", "entry": False,
                "action": "Probe", "classId": "710FB9A8-C47E-4B39-9CFA-E273AB1B78F8", **changes}
        result = subprocess.run(
            [POWERSHELL, "-NoProfile", "-NonInteractive", "-Command", CHILD_CONTROLLER_PROBE],
            env={**os.environ, "CONVT_TEST_MENU_SOURCE": str(HERE / "modern-menu.ps1"),
                 "CONVT_TEST_MENU_CASE": json.dumps(case)},
            capture_output=True, text=True, encoding="utf-8", timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return json.loads(result.stdout)

    def test_timeout_kills_and_reaps_only_the_owned_child_before_failing(self):
        result = self.run_case(timeout=True)
        self.assertFalse(result["result"]["ok"], result)
        self.assertIn("20 seconds", result["result"]["error"])
        self.assertEqual(result["events"], ["start", "wait:20000", "kill", "reap", "dispose"])

    def test_child_failure_exposes_hresult_and_success_uses_fixed_probe_arguments(self):
        result = self.run_case(exitCode=1, stderr="COM activation failed (HRESULT 0x80070005)")
        self.assertFalse(result["result"]["ok"], result)
        self.assertIn("HRESULT 0x80070005", result["result"]["error"])
        self.assertNotIn("kill", result["events"])
        result = self.run_case()
        self.assertTrue(result["result"]["ok"], result)
        self.assertEqual(result["events"], ["start", "wait:20000", "dispose"])
        arguments = result["arguments"]
        self.assertIn('-NoProfile -NonInteractive -File "', arguments)
        self.assertIn("-action probe -classid 710fb9a8-c47e-4b39-9cfa-e273ab1b78f8", arguments.lower())
        self.assertNotIn("executionpolicy", arguments.lower())

    def test_probe_entry_rejects_arbitrary_class_and_never_calls_package_lifecycle(self):
        result = self.run_case(entry=True)
        self.assertTrue(result["result"]["ok"], result)
        self.assertEqual(result["events"], ["native-probe"])
        for changes in ({"classId": "00000000-0000-0000-0000-000000000001"}, {"action": "Install"}):
            with self.subTest(changes=changes):
                result = self.run_case(entry=True, **changes)
                self.assertFalse(result["result"]["ok"], result)
                self.assertEqual(result["events"], [])


class ModernMenuContract(unittest.TestCase):
    def test_identity_manifest_and_scoped_lifecycle_helper_are_delivered(self):
        self.assertTrue((HERE / "AppxManifest.xml").is_file(),
                        "the Windows 11 package identity manifest is missing")
        self.assertTrue((HERE / "modern-menu.ps1").is_file(),
                        "the per-user modern menu lifecycle helper is missing")

    def test_one_file_verb_uses_the_existing_dll_without_duplicate_app_entry(self):
        ns = {"p": "http://schemas.microsoft.com/appx/manifest/foundation/windows10",
              "com": "http://schemas.microsoft.com/appx/manifest/com/windows10",
              "d5": "http://schemas.microsoft.com/appx/manifest/desktop/windows10/5",
              "uap": "http://schemas.microsoft.com/appx/manifest/uap/windows10"}
        manifest = ET.parse(HERE / "AppxManifest.xml").getroot()
        self.assertEqual(manifest.find("p:Identity", ns).attrib,
                         {"Name": "Convt.Desktop", "Publisher": "CN=Convt Local",
                          "Version": "0.3.2.0", "ProcessorArchitecture": "x64"})
        app = manifest.find("p:Applications/p:Application", ns)
        self.assertEqual(app.get("Id"), "ConvtApp")
        self.assertEqual(app.get("Executable"), "convt-app.exe")
        self.assertEqual(app.find("uap:VisualElements", ns).get("AppListEntry"), "none")
        self.assertEqual([item.get("Type") for item in manifest.findall(".//d5:ItemType", ns)], ["*"])
        cls = manifest.find(".//com:Class", ns)
        self.assertEqual(cls.get("Path"), "convt_shell.dll")
        self.assertEqual(cls.get("ThreadingModel"), "STA")
        self.assertEqual(manifest.find(".//d5:Verb", ns).get("Clsid"), cls.get("Id"))


@unittest.skipUnless(os.name == "nt" and POWERSHELL, "requires Windows PowerShell for mocked functions")
class ModernMenuLifecycle(unittest.TestCase):
    def run_case(self, *, steps=None, packages=None, **changes):
        scratch_parent = HERE.parents[1] / "tmp"
        scratch_parent.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="modern-menu-test-", dir=scratch_parent) as folder:
            root = Path(folder) / "Programs with spaces & %" / "convt"
            assets = root / "identity" / "Assets"
            assets.mkdir(parents=True)
            manifest = (HERE / "AppxManifest.xml").read_text(encoding="utf-8")
            for before, after in changes.pop("manifest_changes", []):
                manifest = manifest.replace(before, after)
            (root / "identity" / "AppxManifest.xml").write_text(manifest, encoding="utf-8")
            for name in ("StoreLogo.png", "Square150x150Logo.png", "Square44x44Logo.png"):
                (assets / name).write_bytes(b"\x89PNG\r\n\x1a\n")
            for name in ("convt.exe", "convt-app.exe", "convt_shell.dll"):
                pe = bytearray(160)
                pe[:2] = b"MZ"
                struct.pack_into("<I", pe, 0x3C, 64)
                pe[64:68] = b"PE\0\0"
                struct.pack_into("<H", pe, 68, changes.pop("machine", 0x8664) if name == "convt_shell.dll" else 0x8664)
                struct.pack_into("<H", pe, 86, 0x2000 if name.endswith(".dll") else 0)
                (root / name).write_bytes(pe)
            rows = [{"Name": "Convt.Desktop", "Publisher": "CN=Convt Local", "Version": "0.3.2.0",
                     "InstallLocation": str(root / "identity"), "PackageFullName": FULL, **row}
                    for row in packages or []]
            step_rows = [{"restore": False, "signed": "", "transaction": "00000000-0000-0000-0000-000000000000",
                          "hideFiles": False, "restoreFiles": False, **step}
                         for step in steps or [{"action": "Install"}]]
            case = {"root": str(root), "markerRoot": str(Path(folder) / "journal"), "markerId": TX,
                    "packages": rows, "steps": step_rows, "legacy": True, "badLegacy": False,
                    "build": 26100, "developer": True, "registerFail": False, "activationFail": False, "removeLegacyFail": False,
                    "restoreLegacyFail": False, "externalError": False, "external": "", **changes}
            result = subprocess.run([POWERSHELL, "-NoProfile", "-NonInteractive", "-Command", PROBE],
                                    env={**os.environ, "CONVT_TEST_MENU_SOURCE": str(HERE / "modern-menu.ps1"),
                                         "CONVT_TEST_MENU_CASE": json.dumps(case)},
                                    capture_output=True, text=True, encoding="utf-8", timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return json.loads(result.stdout)

    def test_success_removes_classic_only_after_registration(self):
        result = self.run_case()
        self.assertTrue(result["results"][0]["ok"], result)
        self.assertEqual(result["events"], ["register", "activate-modern", "remove-classic"])
        self.assertEqual(result["packages"], [FULL])
        self.assertFalse(result["legacy"])

    def test_registration_failure_preserves_classic(self):
        result = self.run_case(registerFail=True)
        self.assertFalse(result["results"][0]["ok"])
        self.assertEqual(result["events"], ["register"])
        self.assertTrue(result["legacy"])
        self.assertEqual(result["packages"], [])

    def test_com_activation_failure_preserves_classic_and_compensates_only_new_package(self):
        for packages, expected, events in (
            (None, [], ["register", "activate-modern", "unregister:" + FULL]),
            ([{}], [FULL], ["activate-modern"]),
        ):
            with self.subTest(preexisting=bool(packages)):
                result = self.run_case(activationFail=True, packages=packages,
                                       steps=[{"action": "Install", "transaction": TX}])
                self.assertFalse(result["results"][0]["ok"], result)
                self.assertIn("HRESULT 0x80070005", result["results"][0]["error"])
                self.assertEqual(result["events"], events)
                self.assertNotIn("remove-classic", result["events"])
                self.assertTrue(result["legacy"])
                self.assertEqual(result["packages"], expected)
                self.assertFalse(result["marker"])

    def test_partial_classic_failure_compensates_both_resources(self):
        result = self.run_case(removeLegacyFail=True)
        self.assertFalse(result["results"][0]["ok"])
        self.assertTrue(result["legacy"])
        self.assertEqual(result["packages"], [])
        self.assertEqual(result["events"], ["register", "activate-modern", "remove-classic", "restore-classic", "unregister:" + FULL])
        result = self.run_case(removeLegacyFail=True, restoreLegacyFail=True)
        self.assertFalse(result["results"][0]["ok"])
        self.assertEqual(result["packages"], [], "failed classic compensation must not leave a new Appx behind")

    def test_guards_fail_before_system_mutation(self):
        cases = [{"developer": False}, {"build": 21999}, {"badLegacy": True}, {"machine": 0x14C},
                 {"manifest_changes": [('Publisher="CN=Convt Local"', 'Publisher="CN=Another"')]},
                 {"manifest_changes": [('Type="*"', 'Type="Directory"')]},
                 {"manifest_changes": [('Path="convt_shell.dll"', 'Path="..\\foreign.dll"')]},
                 {"manifest_changes": [('Version="0.3.2.0"', 'Version="65536.0.0.0"')]},
                 {"manifest_changes": [('</com:ComServer>', '<com:ProgId Id="Another.Product" Clsid="710FB9A8-C47E-4B39-9CFA-E273AB1B78F8"/></com:ComServer>')]}]
        for case in cases:
            with self.subTest(case=case):
                result = self.run_case(**case)
                self.assertFalse(result["results"][0]["ok"], result)
                self.assertEqual(result["events"], [])
                self.assertTrue(result["legacy"])

    def test_remove_filters_publisher_and_checks_external_root(self):
        foreign = {"Publisher": "CN=Another", "PackageFullName": "Convt.Desktop_0.3.2.0_x64__foreign"}
        result = self.run_case(packages=[{}, foreign], steps=[{"action": "Remove"}])
        self.assertTrue(result["results"][0]["ok"], result)
        self.assertEqual(result["packages"], [foreign["PackageFullName"]])
        result = self.run_case(packages=[{}], external="C:\\another-install", steps=[{"action": "Remove"}])
        self.assertFalse(result["results"][0]["ok"])
        self.assertEqual(result["events"], [])
        self.assertEqual(result["packages"], [FULL])

    def test_existing_package_wrong_version_or_external_path_is_preserved(self):
        for changes in ({"packages": [{"Version": "0.3.1.0"}]}, {"packages": [{}], "external": "C:\\another-install"},
                        {"packages": [{}], "externalError": True}, {"packages": [{}, {}]}):
            with self.subTest(changes=changes):
                result = self.run_case(**changes)
                self.assertFalse(result["results"][0]["ok"])
                self.assertEqual(result["events"], [])

    def test_install_rollback_removes_new_package_and_preserves_preexisting_package(self):
        for packages, expected in ((None, []), ([{}], [FULL])):
            result = self.run_case(packages=packages, steps=[{"action": "Install", "transaction": TX},
                                                             {"action": "Rollback", "transaction": TX}])
            self.assertTrue(all(step["ok"] for step in result["results"]), result)
            self.assertEqual(result["packages"], expected)
            self.assertTrue(result["legacy"])
            self.assertFalse(result["marker"])

    def test_commit_after_files_removed_and_wrong_rollback_are_scoped(self):
        result = self.run_case(packages=[{}], legacy=False,
                               steps=[{"action": "Remove", "transaction": TX, "hideFiles": True},
                                      {"action": "Commit", "transaction": TX}])
        self.assertTrue(all(step["ok"] for step in result["results"]), result)
        self.assertFalse(result["marker"])
        other = "345a4a89-c6cc-4d8b-b3a2-3f5e54d697e0"
        result = self.run_case(steps=[{"action": "Install", "transaction": TX},
                                     {"action": "Rollback", "transaction": other},
                                     {"action": "Commit", "transaction": TX}])
        self.assertTrue(all(step["ok"] for step in result["results"]), result)
        self.assertFalse(result["results"][1]["result"]["Changed"])
        self.assertEqual(result["packages"], [FULL])

    def test_remove_rollback_restores_modern_package_after_msi_restores_files(self):
        result = self.run_case(packages=[{}], legacy=False,
                               steps=[{"action": "Remove", "transaction": TX, "hideFiles": True},
                                      {"action": "Rollback", "transaction": TX, "restoreFiles": True}])
        self.assertTrue(all(step["ok"] for step in result["results"]), result)
        self.assertEqual(result["packages"], [FULL])
        self.assertFalse(result["legacy"])
        self.assertFalse(result["marker"])

    def test_future_version_and_signed_path_fail_closed(self):
        result = self.run_case(manifest_changes=[('Version="0.3.2.0"', 'Version="0.3.3.0"')])
        self.assertTrue(result["results"][0]["ok"], result)
        self.assertEqual(result["packages"], ["Convt.Desktop_0.3.3.0_x64__fixture"])
        result = self.run_case(steps=[{"action": "Install", "signed": "C:\\unvalidated.msix"}])
        self.assertFalse(result["results"][0]["ok"])
        self.assertEqual(result["events"], [])

    def test_remove_can_explicitly_restore_classic_and_status_is_readonly(self):
        result = self.run_case(steps=[{"action": "Install"}, {"action": "Remove", "restore": True}])
        self.assertTrue(all(step["ok"] for step in result["results"]), result)
        self.assertEqual(result["packages"], [])
        self.assertTrue(result["legacy"])
        result = self.run_case(steps=[{"action": "Status"}])
        self.assertTrue(result["results"][0]["ok"], result)
        self.assertFalse(result["results"][0]["result"]["Registered"])
        self.assertEqual(result["events"], [])

if __name__ == "__main__":
    unittest.main()
