#!/usr/bin/env python3
"""Render the Windows icon and package identity logos from convt-icon.svg.

The convt CLI's resvg engine draws every size from the vector, so the small
frames are rasterized at their own size rather than downscaled. The ICO wraps
those PNG frames unchanged.
"""

import argparse
import os
from pathlib import Path
import struct
import subprocess
import tempfile

PNG = b"\x89PNG\r\n\x1a\n"
# Explorer, the taskbar and Settings pick 16-48 at 100-200% scaling.
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
IDENTITY = [("Square44x44Logo.png", 44), ("Square150x150Logo.png", 150), ("StoreLogo.png", 50)]


def render(convt, source, size, work):
    out = work / str(size)
    out.mkdir()
    # Private config and data, so the builder's own license or trial never
    # decides whether packaging can render the icons.
    env = {**os.environ, "CONVT_LICENSE_STORE": "file",
           "CONVT_CONFIG_DIR": str(work / "config"), "CONVT_DATA_DIR": str(work / "data")}
    # The SVG declares 48 px at 96 DPI, so 2N DPI yields N px.
    subprocess.run([str(convt), str(source), "--to", "png", "--dpi", str(size * 2),
                    "--out-dir", str(out)], check=True, capture_output=True, env=env, timeout=60)
    data = (out / f"{source.stem}.png").read_bytes()
    width, height, _, color = struct.unpack_from(">IIBB", data, 16)
    if data[:8] != PNG or (width, height, color) != (size, size, 6):
        raise ValueError(f"Expected a {size}x{size} RGBA PNG, got {width}x{height} type {color}")
    return data


def ico(frames):
    offset = 6 + 16 * len(frames)
    directory = []
    for size, data in frames:
        directory.append(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset))
        offset += len(data)
    return struct.pack("<HHH", 0, 1, len(frames)) + b"".join(directory) + b"".join(d for _, d in frames)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--convt", type=Path, required=True, help="a built convt CLI")
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    convt = args.convt.resolve(strict=True)
    source = here / "convt-icon.svg"
    assets = here / "Assets"
    assets.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory() as temp:
        work = Path(temp)
        frames = [(size, render(convt, source, size, work)) for size in ICO_SIZES]
        (here / "convt.ico").write_bytes(ico(frames))
        for name, size in IDENTITY:
            (assets / name).write_bytes(render(convt, source, size, work))
    print(f"Built convt.ico ({len(frames)} frames) and {len(IDENTITY)} identity logos")


if __name__ == "__main__":
    main()
