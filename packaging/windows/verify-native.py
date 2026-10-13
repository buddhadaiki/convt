#!/usr/bin/env python3
"""Check packaged HEIF startup in fresh processes without installing or opening UI."""

import argparse
import ctypes
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def initialize_heif(payload):
    # Suppress crash dialogs only in this isolated probe process.
    ctypes.WinDLL("kernel32").SetErrorMode(0x0001 | 0x0002)
    library = ctypes.CDLL(str(payload / "heif.dll"), winmode=0x100 | 0x800)

    class Error(ctypes.Structure):
        _fields_ = [("code", ctypes.c_int), ("subcode", ctypes.c_int),
                    ("message", ctypes.c_char_p)]

    library.heif_convt_init_no_plugins.argtypes = []
    library.heif_convt_init_no_plugins.restype = Error
    error = library.heif_convt_init_no_plugins()
    if error.code:
        raise RuntimeError(f"HEIF initialization failed: {error.code}/{error.subcode}")
    library.heif_deinit.argtypes = []
    library.heif_deinit.restype = None
    library.heif_deinit()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("payload", type=Path)
    parser.add_argument("--probe-heif", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("requires Windows")
    payload = args.payload.resolve(strict=True)
    for name in ("heif.dll", "libde265.dll", "convt.exe"):
        if not (payload / name).is_file():
            parser.error(f"missing packaged runtime: {name}")
    if args.probe_heif:
        initialize_heif(payload)
        return

    with tempfile.TemporaryDirectory(prefix="convt-native-check-") as temporary:
        work = Path(temporary)
        env = dict(os.environ)
        for name in ("CONVT_LIBHEIF_DIR", "CONVT_LIBHEIF_PLUGIN_DIR", "LIBHEIF_PLUGIN_PATH",
                     "CONVT_FFMPEG", "CONVT_FFPROBE", "CONVT_PDFIUM_DIR", "CONVT_SOFFICE",
                     "CONVT_DOCUMENT_PACK"):
            env.pop(name, None)
        env.update(CONVT_CONFIG_DIR=str(work / "config"), CONVT_DATA_DIR=str(work / "data"),
                   CONVT_LICENSE_STORE="file", TEMP=str(work), TMP=str(work))
        for _ in range(3):
            commands = [
                [sys.executable, str(Path(__file__).resolve()), str(payload), "--probe-heif"],
                [str(payload / "convt.exe"), "engines"],
            ]
            for command in commands:
                result = subprocess.run(command, capture_output=True, text=True, env=env,
                                        timeout=30, creationflags=subprocess.CREATE_NO_WINDOW)
                if result.returncode:
                    raise RuntimeError(f"Native startup exited {result.returncode}: "
                                       + result.stdout + result.stderr)
                if command[-1] == "engines" and "libheif" not in result.stdout.splitlines():
                    raise RuntimeError("Packaged HEIF engine is unavailable: " + result.stdout)
    print("PASS: HEIF load/init and packaged CLI engine startup in 6 fresh processes")


if __name__ == "__main__":
    main()
