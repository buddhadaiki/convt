"""Check identity staging independently of package registration."""

import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

from prepare_identity import ASSETS, FOUNDATION, prepare


class PrepareIdentity(unittest.TestCase):
    def test_version_mapping_and_runtime_assets(self):
        with tempfile.TemporaryDirectory() as raw:
            payload = Path(raw)
            (payload / "convt-app.exe").write_bytes(b"app")
            (payload / "convt_shell.dll").write_bytes(b"shell")
            manifest = prepare(payload, "0.3.2")
            identity = ET.parse(manifest).getroot().find(f"{{{FOUNDATION}}}Identity")
            self.assertEqual(identity.attrib["Version"], "0.3.2.0")
            self.assertEqual(identity.attrib["Name"], "Convt.Desktop")
            self.assertEqual(identity.attrib["Publisher"], "CN=Convt Local")
            self.assertTrue((payload / "identity" / "modern-menu.ps1").is_file())
            for name in ASSETS:
                self.assertTrue((payload / "identity" / "Assets" / name).is_file())
            updated = prepare(payload, "1.4.27")
            self.assertEqual(ET.parse(updated).getroot().find(
                f"{{{FOUNDATION}}}Identity").attrib["Version"], "1.4.27.0")

    def test_invalid_version_or_incomplete_runtime_does_not_stage(self):
        with tempfile.TemporaryDirectory() as raw:
            payload = Path(raw)
            for version in ("1.2", "1.2.3.4", "256.0.0", "0.0.65536", "00.03.05", "0.03.5", "0.3.05"):
                with self.assertRaises(ValueError):
                    prepare(payload, version)
            with self.assertRaises(ValueError):
                prepare(payload, "0.3.2")
            self.assertFalse((payload / "identity").exists())


if __name__ == "__main__":
    unittest.main()
