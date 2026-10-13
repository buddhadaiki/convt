"""Inspect a compiled MSI without installing it or changing the registry.

Set CONVT_TEST_WIX_DLL to the pinned WiX CLI DLL and CONVT_TEST_DOTNET
to dotnet.exe. Other hosts and checkouts without WiX skip these tests.
"""

import ctypes
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
WIX = os.environ.get("CONVT_TEST_WIX_DLL")
DOTNET = os.environ.get("CONVT_TEST_DOTNET", "dotnet")


@unittest.skipUnless(os.name == "nt" and WIX, "requires Windows and pinned WiX")
class UrlProtocol(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.temp.cleanup)
        root = Path(cls.temp.name)
        payload = root / "payload"
        payload.mkdir()
        # WiX needs files to harvest, but these tests never launch the payload.
        for name in ("convt.exe", "convt-app.exe", "convt_shell.dll"):
            (payload / name).write_bytes(b"test payload")
        actions = os.environ.get("CONVT_TEST_INSTALLER_ACTIONS_DLL")
        if not actions:
            # Authoring-only fixture: the Binary is never loaded or executed.
            actions = root / "actions-never-invoked.dll"
            actions.write_bytes(b"read-only MSI authoring fixture")
        cls.msi_path = root / "convt-test.msi"
        result = subprocess.run(
            [DOTNET, WIX, "build", "-arch", "x64", "-d", "Version=0.3.0",
             "-d", f"Payload={payload}", "-d", f"InstallerActionsDll={actions}", str(HERE / "convt.wxs"),
             "-o", str(cls.msi_path)],
            env={**os.environ, "DOTNET_ROLL_FORWARD": "Major"},
            capture_output=True, text=True, timeout=60,
        )
        if result.returncode:
            raise AssertionError(result.stdout + result.stderr)
        cls.api = ctypes.WinDLL("msi")
        cls.api.MsiOpenDatabaseW.argtypes = [ctypes.c_wchar_p, ctypes.c_void_p,
                                           ctypes.POINTER(ctypes.c_uint)]
        cls.api.MsiDatabaseOpenViewW.argtypes = [ctypes.c_uint, ctypes.c_wchar_p,
                                               ctypes.POINTER(ctypes.c_uint)]
        cls.api.MsiViewExecute.argtypes = [ctypes.c_uint, ctypes.c_uint]
        cls.api.MsiViewFetch.argtypes = [ctypes.c_uint, ctypes.POINTER(ctypes.c_uint)]
        cls.api.MsiRecordGetStringW.argtypes = [ctypes.c_uint, ctypes.c_uint,
                                              ctypes.c_wchar_p,
                                              ctypes.POINTER(ctypes.c_uint)]
        cls.api.MsiCloseHandle.argtypes = [ctypes.c_uint]
        cls.database = ctypes.c_uint()
        status = cls.api.MsiOpenDatabaseW(str(cls.msi_path), None,
                                         ctypes.byref(cls.database))
        if status:
            raise OSError(status, "MsiOpenDatabaseW")
        cls.addClassCleanup(cls.api.MsiCloseHandle, cls.database)

    def query(self, sql, columns):
        view = ctypes.c_uint()
        self.assertEqual(self.api.MsiDatabaseOpenViewW(
            self.database, sql, ctypes.byref(view)), 0)
        try:
            self.assertEqual(self.api.MsiViewExecute(view, 0), 0)
            rows = []
            while True:
                record = ctypes.c_uint()
                status = self.api.MsiViewFetch(view, ctypes.byref(record))
                if status == 259:  # ERROR_NO_MORE_ITEMS
                    return rows
                self.assertEqual(status, 0)
                try:
                    row = []
                    for column in range(1, columns + 1):
                        value = ctypes.create_unicode_buffer(4096)
                        length = ctypes.c_uint(len(value))
                        self.assertEqual(self.api.MsiRecordGetStringW(
                            record, column, value, ctypes.byref(length)), 0)
                        row.append(value.value)
                    rows.append(row)
                finally:
                    self.api.MsiCloseHandle(record)
        finally:
            self.api.MsiCloseHandle(view)

    def test_protocol_is_per_user_and_reaches_the_gui_with_one_url_argument(self):
        rows = self.query("SELECT `Root`, `Key`, `Name`, `Value`, `Component_` "
                          "FROM `Registry`", 5)
        protocol = [row for row in rows if row[1].startswith("Software\\Classes\\convt")]
        values = {(key, name): value for _, key, name, value, _ in protocol}
        self.assertEqual({row[0] for row in protocol}, {"1"})  # HKCU
        self.assertEqual(values.get(("Software\\Classes\\convt", "URL Protocol")), "")
        self.assertTrue(values.get(("Software\\Classes\\convt", ""), "").startswith("URL:"))
        command = values.get(("Software\\Classes\\convt\\shell\\open\\command", ""))
        self.assertIsNotNone(command, "MSI must register the callback command")
        shell = ctypes.WinDLL("shell32")
        shell.CommandLineToArgvW.argtypes = [ctypes.c_wchar_p,
                                            ctypes.POINTER(ctypes.c_int)]
        shell.CommandLineToArgvW.restype = ctypes.POINTER(ctypes.c_wchar_p)
        kernel = ctypes.WinDLL("kernel32")
        kernel.LocalFree.argtypes = [ctypes.c_void_p]
        for url in ("convt://auth?state=abc&code=123",
                    "convt://activate?key=encoded%20key",
                    "convt://convert?file=C%3A%5CMy%20Files%5Cphoto.png&to=webp"):
            with self.subTest(url=url):
                install = "C:\\Users\\Test User\\AppData\\Local\\Programs\\convt\\"
                expanded = command.replace("[INSTALLFOLDER]", install).replace("%1", url)
                count = ctypes.c_int()
                args = shell.CommandLineToArgvW(expanded, ctypes.byref(count))
                self.assertTrue(args)
                try:
                    self.assertEqual([args[i] for i in range(count.value)],
                                     [install + "convt-app.exe", url])
                finally:
                    kernel.LocalFree(args)

    def test_protocol_component_is_installed_and_removed_with_the_product(self):
        rows = self.query("SELECT `Component_` FROM `Registry` "
                          "WHERE `Key` = 'Software\\Classes\\convt'", 1)
        self.assertTrue(rows, "protocol component is missing")
        components = {row[0] for row in rows}
        selected = {row[0] for row in self.query(
            "SELECT `Component_` FROM `FeatureComponents` WHERE `Feature_` = 'Main'", 1)}
        self.assertTrue(components <= selected)
        attributes = dict(self.query("SELECT `Component`, `Attributes` FROM `Component`", 2))
        for component in components:
            self.assertFalse(int(attributes[component]) & 16, "component must not be permanent")
            self.assertTrue(int(attributes[component]) & 4, "per-user registry must be its key path")
        actions = {row[0] for row in self.query("SELECT `Action` FROM `InstallExecuteSequence`", 1)}
        self.assertTrue({"WriteRegistryValues", "RemoveRegistryValues"} <= actions)


if __name__ == "__main__":
    unittest.main()
