# convt brand

The mark is two overlapping rounded squares: the file you have (ink) and the file you get (blue). The overlap, in a third shade, is where the conversion happens.

| File                                        | Use                                                                              |
| ------------------------------------------- | -------------------------------------------------------------------------------- |
| `mark-on-dark.svg`, `mark-on-light.svg`     | The mark alone, for dark and light backgrounds.                                  |
| `lockup-on-dark.png`, `lockup-on-light.png` | Mark plus the "convt" wordmark, 2400 px wide on a transparent background.        |
| `../../packaging/linux/convt.svg`           | The app icon (the mark on a dark tile). Linux and macOS packaging build from it. |
| `../../packaging/windows/convt-icon.svg`    | The same icon with a full-canvas tile, for Windows' `convt.ico` and logos.       |
| `../../apps/web/public/favicon.svg`         | The favicon. It switches colors for light and dark browser tabs.                 |

The wordmark is "convt" in lowercase Geist SemiBold with -0.02em tracking. Beside it, the mark is about 1.2 times the text's font size, with a gap of half the font size.

## Colors

| Role                            | On dark                | On light               |
| ------------------------------- | ---------------------- | ---------------------- |
| Source square (ink)             | `#eeeef0`              | `#0a0a0a`              |
| Converted square, top to bottom | `#6595ff` to `#4c82ff` | `#6092ff` to `#325ec7` |
| Overlap                         | `#a5c3ff`              | `#244698`              |
| Page                            | `#0a0b0b`              | `#ffffff`              |

The site defines these as `--mark-*` tokens in `apps/web/src/styles.css`; `apps/web/src/components/logo.tsx` draws the mark from them.
