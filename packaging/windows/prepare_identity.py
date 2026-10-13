#!/usr/bin/env python3
"""Stage the external-location identity manifest and assets beside the runtime."""

import argparse
from pathlib import Path
import re
import shutil
import xml.etree.ElementTree as ET

FOUNDATION = "http://schemas.microsoft.com/appx/manifest/foundation/windows10"
ASSETS = ("Square44x44Logo.png", "Square150x150Logo.png", "StoreLogo.png")


def prepare(payload, version):
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version):
        raise ValueError("Expected MSI major.minor.build version")
    numbers = [int(value) for value in version.split(".")]
    if any(value > limit for value, limit in zip(numbers, (255, 255, 65535))):
        raise ValueError("MSI version component exceeds its limit")
    here = Path(__file__).resolve().parent
    payload = payload.resolve(strict=True)
    if not (payload / "convt-app.exe").is_file() or not (payload / "convt_shell.dll").is_file():
        raise ValueError("Verified app and Explorer DLL are required before identity staging")
    manifest = (here / "AppxManifest.xml").read_text(encoding="utf-8-sig")
    root = ET.fromstring(manifest)
    identity = root.find(f"{{{FOUNDATION}}}Identity")
    if identity is None or identity.attrib.get("Name") != "Convt.Desktop":
        raise ValueError("Unexpected identity template")
    original_version = identity.attrib["Version"]
    # Keep the authored namespaces and whitespace rather than reserializing them.
    updated, count = re.subn(r'(?<=Version=")' + re.escape(original_version) + r'(?=")',
                             version + ".0", manifest, count=1)
    if count != 1:
        raise ValueError("Cannot set identity version")
    destination = payload / "identity"
    destination.mkdir(exist_ok=True)
    (destination / "AppxManifest.xml").write_text(updated, encoding="utf-8")
    shutil.copy2(here / "modern-menu.ps1", destination / "modern-menu.ps1")
    assets = destination / "Assets"
    assets.mkdir(exist_ok=True)
    for name in ASSETS:
        shutil.copy2(here / "Assets" / name, assets / name)
    return destination / "AppxManifest.xml"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("payload", type=Path)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    print(prepare(args.payload, args.version))


if __name__ == "__main__":
    main()
