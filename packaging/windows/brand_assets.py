#!/usr/bin/env python3
"""Build Windows PNG/ICO containers from the approved convt logo with FFmpeg."""

import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess


def png_info(path):
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        raise ValueError(f"Not a PNG: {path}")
    width, height, depth, color = struct.unpack_from(">IIBB", data, 16)
    return {"width": width, "height": height, "depth": depth, "color_type": color,
            "sha256": hashlib.sha256(data).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ffmpeg", type=Path, required=True)
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    repo = here.parents[1]
    brand = repo / "design-assets/brand/convt-v2"
    source = brand / "logo.png"
    frames = brand / "windows"
    frames.mkdir(parents=True, exist_ok=True)
    assets = here / "Assets"
    assets.mkdir(exist_ok=True)
    generated = {}

    def resize(size, target):
        subprocess.run([str(args.ffmpeg.resolve(strict=True)), "-v", "error", "-nostdin",
                        "-y", "-i", str(source), "-vf", f"scale={size}:{size}:flags=lanczos",
                        "-pix_fmt", "rgba", "-frames:v", "1", "-update", "1", str(target)],
                       check=True, capture_output=True, timeout=30)
        info = png_info(target)
        if (info["width"], info["height"], info["color_type"]) != (size, size, 6):
            raise ValueError(f"Expected square RGBA PNG: {target}")
        generated[target.relative_to(repo).as_posix()] = info

    sizes = [16, 24, 32, 48, 64, 128, 256]
    for size in sizes:
        resize(size, frames / f"icon-{size}.png")
    for name, size in [("Square44x44Logo.png", 44), ("Square150x150Logo.png", 150),
                       ("StoreLogo.png", 50)]:
        resize(size, assets / name)

    # ICO wraps the already-converted PNG frames; pixel data remains unchanged.
    images = [(frames / f"icon-{size}.png").read_bytes() for size in sizes]
    offset = 6 + 16 * len(images)
    directory = []
    for size, data in zip(sizes, images):
        directory.append(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0,
                                     1, 32, len(data), offset))
        offset += len(data)
    icon = here / "convt-v2.ico"
    icon.write_bytes(struct.pack("<HHH", 0, 1, len(images))
                     + b"".join(directory) + b"".join(images))
    generated[icon.relative_to(repo).as_posix()] = {
        "frames": sizes, "sha256": hashlib.sha256(icon.read_bytes()).hexdigest()}
    (brand / "windows-assets.json").write_text(json.dumps(generated, indent=2) + "\n",
                                                encoding="utf-8")
    print(f"Built {len(images)} ICO frames and {len(generated) - 1} RGBA PNG assets")


if __name__ == "__main__":
    main()
