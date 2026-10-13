"""Inspect the Explorer MSI registration without installing or registering it."""

import ctypes
import os
import unittest

import test_url_protocol as protocol

CLSID = "{710FB9A8-C47E-4B39-9CFA-E273AB1B78F8}"
CLASS_KEY = "Software\\Classes\\CLSID\\" + CLSID
VERB_KEY = "Software\\Classes\\*\\shell\\convt"


@unittest.skipUnless(os.name == "nt" and protocol.WIX, "requires Windows and pinned WiX")
class ExplorerMenu(unittest.TestCase):
    # Share the compiled-MSI fixture and read-only Win32 query helper, not the
    # protocol's test cases. No installer, COM registration or GUI is invoked.
    setUpClass = classmethod(protocol.UrlProtocol.setUpClass.__func__)
    query = protocol.UrlProtocol.query

    def test_installed_dll_has_per_user_com_and_file_verb_registration(self):
        rows = self.query("SELECT `Root`, `Key`, `Name`, `Value`, `Component_` "
                          "FROM `Registry`", 5)
        menu = [row for row in rows if row[1].startswith(CLASS_KEY)
                or row[1] == VERB_KEY]
        self.assertTrue(menu, "Explorer registration must be authored in the MSI")
        self.assertEqual({row[0] for row in menu}, {"1"})  # HKCU
        values = {(key, name): value for _, key, name, value, _ in menu}
        self.assertEqual(values.get((CLASS_KEY + "\\InprocServer32", "")),
                         "[INSTALLFOLDER]convt_shell.dll")
        self.assertEqual(values.get((CLASS_KEY + "\\InprocServer32", "ThreadingModel")),
                         "Apartment")
        self.assertEqual(values.get((VERB_KEY, "ExplorerCommandHandler")), CLSID)
        self.assertEqual(values.get((VERB_KEY, "MultiSelectModel")), "Player")
        self.assertEqual(values.get((VERB_KEY, "MUIVerb")), "Convert with convt")
        files = self.query("SELECT `FileName` FROM `File`", 1)
        self.assertIn("convt_shell.dll", [name.split("|")[-1] for [name] in files])

    def test_registration_removes_owned_values_and_preserves_user_data(self):
        rows = self.query("SELECT `Registry`, `Key`, `Name`, `Component_` FROM `Registry`", 4)
        menu = [row for row in rows if row[1].startswith(CLASS_KEY)
                or row[1] == VERB_KEY]
        self.assertTrue(menu)
        components = {row[3] for row in menu}
        selected = {row[0] for row in self.query(
            "SELECT `Component_` FROM `FeatureComponents` WHERE `Feature_` = 'Main'", 1)}
        self.assertTrue(components <= selected)
        component_rows = {name: (int(attributes), key_path) for name, attributes, key_path
                          in self.query("SELECT `Component`, `Attributes`, `KeyPath` "
                                        "FROM `Component`", 3)}
        for component in components:
            attributes, key_path = component_rows[component]
            self.assertTrue(attributes & 4, "HKCU registry must be the key path")
            self.assertTrue(attributes & 256, "the handler must use the x64 registry view")
            self.assertFalse(attributes & 16, "component must not be permanent")
            self.assertIn(key_path, {row[0] for row in menu})
        self.assertFalse(any(row[2] in ("*", "-") for row in rows),
                         "uninstall must not recursively delete registry keys")
        actions = {row[0] for row in self.query("SELECT `Action` FROM `InstallExecuteSequence`", 1)}
        self.assertTrue({"WriteRegistryValues", "RemoveRegistryValues"} <= actions)

    def test_finish_integrations_have_embedded_user_rollback_and_commit(self):
        rows = self.query("SELECT `Action`, `Type`, `Source`, `Target` FROM `CustomAction`", 4)
        expected = {
            "ConvtFinishPrepare": (1, "ConvtInstallerActions"),
            "ConvtFinishApply": (1025, "ConvtInstallerActions"),
            "ConvtFinishRollback": (1281, "ConvtInstallerActions"),
            "ConvtFinishCommit": (1601, "ConvtInstallerActions"),
            "ConvtFinishRemove": (1025, "ConvtInstallerActions"),
            "ConvtFinishRemoveRollback": (1281, "ConvtInstallerActions"),
            "ConvtFinishRemoveCommit": (1601, "ConvtInstallerActions"),
            "ConvtFinishConfigure": (1, "ConvtInstallerActions"),
        }
        actual = {name: (int(kind), source) for name, kind, source, _ in rows}
        self.assertEqual({name: actual.get(name) for name in expected}, expected)
        targets = {name: target for name, _, _, target in rows}
        for name in expected:
            entry = "Rollback" if name == "ConvtFinishRemoveRollback" else (
                "Commit" if name == "ConvtFinishRemoveCommit" else name.removeprefix("ConvtFinish"))
            self.assertEqual(targets[name], entry)
        sequence = {name: (condition, int(position)) for name, condition, position in self.query(
            "SELECT `Action`, `Condition`, `Sequence` FROM `InstallExecuteSequence`", 3)}
        self.assertLess(sequence["InstallInitialize"][1], sequence["RemoveExistingProducts"][1])
        self.assertLess(sequence["InstallInitialize"][1], sequence["ConvtFinishPrepare"][1])
        self.assertLess(sequence["ConvtFinishPrepare"][1], sequence["RemoveExistingProducts"][1])
        self.assertLess(sequence["RemoveExistingProducts"][1], sequence["ConvtFinishRollback"][1])
        self.assertLess(sequence["ConvtFinishRollback"][1], sequence["InstallFiles"][1])
        for name, kind, _, _ in rows:
            if name in sequence and sequence["InstallInitialize"][1] < sequence[name][1] < sequence["RemoveExistingProducts"][1]:
                self.assertFalse(int(kind) & 1024, f"{name} generates a script before early RemoveExistingProducts (error2613)")
        self.assertLess(sequence["RemoveExistingProducts"][1], sequence["InstallFiles"][1])
        self.assertLess(sequence["WriteRegistryValues"][1], sequence["ConvtFinishApply"][1])
        self.assertLess(sequence["ConvtFinishApply"][1], sequence["ConvtFinishCommit"][1])
        self.assertLess(sequence["ConvtFinishRemoveRollback"][1], sequence["ConvtFinishRemove"][1])
        self.assertLess(sequence["ConvtFinishRemove"][1], sequence["RemoveFiles"][1])
        for name in ("ConvtFinishRemoveRollback", "ConvtFinishRemove", "ConvtFinishRemoveCommit"):
            self.assertEqual(sequence[name][0], 'REMOVE~="ALL" AND NOT UPGRADINGPRODUCTCODE')
        for name in ("ConvtFinishRollback", "ConvtFinishApply", "ConvtFinishCommit"):
            self.assertEqual(sequence[name][0], 'NOT (REMOVE~="ALL")')
        self.assertEqual(sequence["ConvtFinishPrepare"][0], 'NOT UPGRADINGPRODUCTCODE')

    def test_classic_checkbox_choice_is_persisted_for_maintenance(self):
        components = {name: (int(attr), condition) for name, attr, condition in self.query(
            "SELECT `Component`, `Attributes`, `Condition` FROM `Component`", 3)}
        self.assertTrue(components["ExplorerVerb"][0] & 64, "classic menu condition must be re-evaluated on repair")
        self.assertEqual(components["ExplorerVerb"][1], 'CONVT_CONTEXT_MENU <> "0"')
        self.assertFalse(components["ExplorerMenu"][1], "COM fallback class must remain installed")

    @unittest.skipUnless(os.environ.get("CONVT_TEST_INSTALLER_ACTIONS_DLL"), "provide real offline action DLL for export validation")
    def test_every_dll_custom_action_entry_is_an_actual_native_export(self):
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.LoadLibraryExW.argtypes = [ctypes.c_wchar_p, ctypes.c_void_p, ctypes.c_uint]
        kernel.LoadLibraryExW.restype = ctypes.c_void_p
        kernel.GetProcAddress.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        kernel.GetProcAddress.restype = ctypes.c_void_p
        kernel.FreeLibrary.argtypes = [ctypes.c_void_p]
        # DllMain only records its HINSTANCE; no export/custom action is called.
        # Resolve all imports from System32, never PATH or an unrelated folder.
        module = kernel.LoadLibraryExW(os.environ["CONVT_TEST_INSTALLER_ACTIONS_DLL"], None, 0x800)
        self.assertTrue(module, ctypes.get_last_error())
        try:
            entries = {target for source, target in self.query(
                "SELECT `Source`, `Target` FROM `CustomAction`", 2) if source == "ConvtInstallerActions"}
            self.assertEqual(entries, {"Prepare", "Apply", "Rollback", "Commit", "Remove", "Configure"})
            for entry in entries:
                self.assertTrue(kernel.GetProcAddress(module, entry.encode("ascii")), entry)
        finally:
            self.assertTrue(kernel.FreeLibrary(module))


if __name__ == "__main__":
    unittest.main()
