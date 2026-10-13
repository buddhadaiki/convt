"""Inspect the finish page in a compiled MSI; never install or launch it."""

import os
import unittest

import test_url_protocol as protocol


@unittest.skipUnless(os.name == "nt" and protocol.WIX, "requires Windows and pinned WiX")
class InstallerFinishUI(unittest.TestCase):
    setUpClass = classmethod(protocol.UrlProtocol.setUpClass.__func__)
    query = protocol.UrlProtocol.query

    def test_menu_and_launch_start_checked_and_desktop_unchecked(self):
        properties = dict(self.query("SELECT `Property`, `Value` FROM `Property`", 2))
        self.assertEqual(properties.get("CONVT_CONTEXT_MENU"), "1")
        self.assertFalse(properties.get("CONVT_DESKTOP_SHORTCUT"))
        self.assertEqual(properties.get("CONVT_LAUNCH_APP"), "1")
        checkbox_values = dict(self.query("SELECT `Property`, `Value` FROM `CheckBox`", 2))
        for prop in ("CONVT_CONTEXT_MENU", "CONVT_DESKTOP_SHORTCUT", "CONVT_LAUNCH_APP"):
            self.assertEqual(checkbox_values.get(prop), "1")

    def test_finish_controls_have_the_requested_labels_and_properties(self):
        controls = {name: (kind, prop, text) for name, kind, prop, text in self.query(
            "SELECT `Control`, `Type`, `Property`, `Text` FROM `Control` "
            "WHERE `Dialog_` = 'ConvtFinish'", 4)}
        self.assertEqual(controls["ContextMenu"], (
            "CheckBox", "CONVT_CONTEXT_MENU",
            "Converter arquivos com o botão direito no Explorador de Arquivos"))
        self.assertEqual(controls["DesktopShortcut"], (
            "CheckBox", "CONVT_DESKTOP_SHORTCUT", "Criar um atalho na Área de Trabalho"))
        self.assertEqual(controls["LaunchApp"], (
            "CheckBox", "CONVT_LAUNCH_APP", "Abrir o convt agora"))
        self.assertEqual(controls["Finish"][::2], ("PushButton", "Concluir"))
        # One way out of the page: opening the app is a choice, not a second button.
        buttons = [name for name, (kind, _, _) in controls.items() if kind == "PushButton"]
        self.assertEqual(buttons, ["Finish"])

    def test_every_page_is_headed_by_the_app_icon(self):
        rows = self.query("SELECT `Dialog_`, `Control`, `Type`, `Text` FROM `Control` "
                          "WHERE `Type` = 'Icon'", 4)
        self.assertEqual({dialog for dialog, _, _, _ in rows},
                         {"ConvtProgress", "ConvtFinish", "ConvtFailed", "ConvtCancelled"})
        self.assertEqual({text for _, _, _, text in rows}, {"ConvtMark"})
        binaries = {name for [name] in self.query("SELECT `Name` FROM `Binary`", 1)}
        self.assertIn("ConvtMark", binaries)

    def test_choices_are_applied_before_launch_and_close(self):
        events = {(event, arg): (condition, int(order)) for event, arg, condition, order in self.query(
            "SELECT `Event`, `Argument`, `Condition`, `Ordering` FROM `ControlEvent` "
            "WHERE `Dialog_` = 'ConvtFinish' AND `Control_` = 'Finish'", 4)}
        apply = events[("DoAction", "ConvtFinishConfigure")][1]
        launch_condition, launch = events[("DoAction", "ConvtLaunchApp")]
        close = events[("EndDialog", "Return")][1]
        self.assertLess(apply, launch)
        self.assertLess(launch, close)
        self.assertIn('CONVT_FINISH_APPLIED = "1"', launch_condition)
        self.assertIn('CONVT_LAUNCH_APP = "1"', launch_condition)
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
