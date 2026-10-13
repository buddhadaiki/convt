# convt — logo v2

New Windows app identity: two emerald/mint conversion arrows form a rounded C on a charcoal tile. The surrounding pixels are transparent. The original upstream brand assets are preserved.

The master `logo.png` was generated with the built-in image generation tool. Windows PNG sizes and the multi-frame ICO are derived by `packaging/windows/brand_assets.py` using the bundled FFmpeg; it changes only dimensions and file containers.

Prompt used:

> Use case: logo-brand. Create one polished app icon / logo mark for convt, a private local file converter for Windows. A friendly, distinctive rounded C made from two interlocking curved conversion arrows / folded ribbons: emerald green and soft mint, matching an existing green-and-charcoal identity. Strong clean silhouette, balanced geometry, generous negative space, readable at 24px. The symbol sits centered on a charcoal rounded-square app tile, with a small restrained dimensional edge and soft material shading, not neon or glossy plastic. Crisp professional contemporary desktop software icon, warm and approachable. Square composition with the entire tile visible, symmetrical empty transparent margin around the tile. No letters, words, file extensions, small decorations, watermarks or mockup scene. Genuine transparent background outside the icon. Deliver a single isolated icon, not a grid or presentation board.

Rebuild Windows assets from this repository:

```powershell
python packaging/windows/brand_assets.py --ffmpeg packaging/out/windows/payload/ffmpeg.exe
```

`windows-assets.json` records dimensions, alpha-capable PNG color type, ICO frames and SHA-256 digests.
