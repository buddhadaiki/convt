"""Inspect the finish page in a compiled MSI; never install or launch it."""

import os
import unittest

import test_url_protocol as protocol


@unittest.skipUnless(os.name == "nt" and protocol.WIX, "requires Windows and pinned WiX")
class InstallerFinishUI(unittest.TestCase):
    setUpClass = classmethod(protocol.UrlProtocol.setUpClass.__func__)
    query = protocol.UrlProtocol.query

    def test_menu_starts_checked_and_desktop_unchecked(self):
        properties = dict(self.query("SELECT `Property`, `Value` FROM `Property`", 2))
        self.assertEqual(properties.get("CONVT_CONTEXT_MENU"), "1")
        self.assertFalse(properties.get("CONVT_DESKTOP_SHORTCUT"))
        checkbox_values = dict(self.query("SELECT `Property`, `Value` FROM `CheckBox`", 2))
        self.assertEqual(checkbox_values.get("CONVT_CONTEXT_MENU"), "1")
        self.assertEqual(checkbox_values.get("CONVT_DESKTOP_SHORTCUT"), "1")

    def test_finish_controls_have_the_requested_labels_and_properties(self):
        controls = {name: (kind, prop, text) for name, kind, prop, text in self.query(
            "SELECT `Control`, `Type`, `Property`, `Text` FROM `Control` "
            "WHERE `Dialog_` = 'ConvtFinish'", 4)}
        self.assertEqual(controls["ContextMenu"], (
            "CheckBox", "CONVT_CONTEXT_MENU", "Adicionar ao menu de contexto"))
        self.assertEqual(controls["DesktopShortcut"], (
            "CheckBox", "CONVT_DESKTOP_SHORTCUT", "Adicionar à Área de Trabalho"))
        self.assertEqual(controls["Open"][::2], ("PushButton", "Abrir convt"))
        self.assertEqual(controls["Finish"][::2], ("PushButton", "Concluir"))

    def test_choices_are_applied_before_launch_or_close(self):
        for button, final_event, final_argument in (
            ("Open", "DoAction", "ConvtLaunchApp"),
            ("Finish", "EndDialog", "Return"),
        ):
            events = self.query(
                "SELECT `Event`, `Argument`, `Ordering` FROM `ControlEvent` "
                f"WHERE `Dialog_` = 'ConvtFinish' AND `Control_` = '{button}'", 3)
            apply = next(int(order) for event, arg, order in events
                         if event == "DoAction" and arg == "ConvtFinishConfigure")
            complete = next(int(order) for event, arg, order in events
                            if event == final_event and arg == final_argument)
            self.assertLess(apply, complete)
        actions = {name: int(kind) for name, kind in self.query(
            "SELECT `Action`, `Type` FROM `CustomAction`", 2)}
        self.assertIn("ConvtFinishConfigure", actions)
        self.assertFalse(actions["ConvtFinishConfigure"] & 64,
                         "configuration failures must not be ignored")
        self.assertTrue(actions["ConvtLaunchApp"] & 128,
                        "the installer must not wait for the opened app to exit")

    def test_finish_page_is_only_in_the_success_path_after_execution(self):
        sequence = {name: (condition, int(position)) for name, condition, position in self.query(
            "SELECT `Action`, `Condition`, `Sequence` FROM `InstallUISequence`", 3)}
        self.assertEqual(sequence["ConvtFinish"][1], -1)
        self.assertIn('REMOVE~="ALL"', sequence["ConvtFinish"][0])
        self.assertGreater(sequence["ExecuteAction"][1], sequence["ConvtProgress"][1])
        self.assertEqual(sequence["ConvtFailed"][1], -3)
        self.assertEqual(sequence["ConvtCancelled"][1], -2)

    def test_stored_unchecked_choices_are_null_only_after_execution(self):
        sequence = {name: (condition, int(position)) for name, condition, position in self.query(
            "SELECT `Action`, `Condition`, `Sequence` FROM `InstallUISequence`", 3)}
        actions = {name: (int(kind), source, target) for name, kind, source, target in self.query(
            "SELECT `Action`, `Type`, `Source`, `Target` FROM `CustomAction`", 4)}
        for prop in ("CONVT_CONTEXT_MENU", "CONVT_DESKTOP_SHORTCUT"):
            action = "Set" + prop
            self.assertEqual(actions[action], (51, prop, ""))
            self.assertEqual(sequence[action][0], prop + ' = "0"')
            self.assertGreater(sequence[action][1], sequence["ExecuteAction"][1])


if __name__ == "__main__":
    unittest.main()
