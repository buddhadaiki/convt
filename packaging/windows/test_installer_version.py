"""Check MSI version selection without running the installer, WiX or registry actions."""

import json
import os
import shutil
import subprocess
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
POWERSHELL = shutil.which("pwsh") or shutil.which("powershell")
PROBE = r"""
$ErrorActionPreference = 'Stop'
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    $env:CONVT_TEST_INSTALLER_SOURCE, [ref]$tokens, [ref]$errors)
if ($errors) { throw 'Installer script has syntax errors' }
$function = $ast.Find({ param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Resolve-MsiInstallerVersion'
}, $true)
if (-not $function) { throw 'MSI version selector is missing' }
# Import only the pure function. Never execute the installer's top-level code.
. ([scriptblock]::Create($function.Extent.Text))
$cases = $env:CONVT_TEST_MSI_VERSION_CASES | ConvertFrom-Json
$result = @($cases | ForEach-Object {
    try {
        $version = Resolve-MsiInstallerVersion -InstallerVersion $_.requested -AppVersion $_.app
        @{ ok = $true; version = $version }
    } catch {
        @{ ok = $false; error = $_.Exception.Message }
    }
})
ConvertTo-Json -InputObject $result -Compress
"""


@unittest.skipUnless(os.name == "nt" and POWERSHELL, "requires Windows PowerShell")
class InstallerVersion(unittest.TestCase):
    def resolve(self, cases):
        result = subprocess.run(
            [POWERSHELL, "-NoProfile", "-NonInteractive", "-Command", PROBE],
            env={**os.environ, "CONVT_TEST_INSTALLER_SOURCE": str(HERE / "installer.ps1"),
                 "CONVT_TEST_MSI_VERSION_CASES": json.dumps(cases)},
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return json.loads(result.stdout)

    def test_default_app_version_and_explicit_installer_revision(self):
        rows = self.resolve([
            {"requested": None, "app": "0.3.0"},
            {"requested": "0.3.1", "app": "0.3.0"},
        ])
        self.assertEqual(rows, [{"ok": True, "version": "0.3.0"},
                                {"ok": True, "version": "0.3.1"}])

    def test_msi_component_boundaries_are_accepted(self):
        versions = ["0.0.0", "255.255.65535", "1.2.3"]
        rows = self.resolve([{"requested": version, "app": "0.3.0"}
                             for version in versions])
        self.assertEqual(rows, [{"ok": True, "version": version} for version in versions])

    def test_bad_versions_fail_before_any_installer_operation(self):
        versions = ["1", "1.2", "1.2.3.4", "1.2.3-beta", "1.2.-1", " 1.2.3",
                    "1.2.3\n", "256.0.0", "0.256.0", "0.0.65536",
                    "999999999999999999999.0.0", "00.03.05", "0.03.5", "0.3.05"]
        rows = self.resolve([{"requested": version, "app": "0.3.0"}
                             for version in versions])
        self.assertEqual(len(rows), len(versions))
        for row in rows:
            self.assertFalse(row["ok"])
            self.assertIn("MSI installer version", row["error"])

    def test_default_app_version_is_validated_too(self):
        rows = self.resolve([{"requested": None, "app": "256.0.0"}])
        self.assertFalse(rows[0]["ok"])


if __name__ == "__main__":
    unittest.main()
