# repo-health

[![Release](https://github.com/skunpro/repo-health/actions/workflows/release.yml/badge.svg)](https://github.com/skunpro/repo-health/actions/workflows/release.yml)
[![CI](https://github.com/skunpro/repo-health/actions/workflows/ci.yml/badge.svg)](https://github.com/skunpro/repo-health/actions/workflows/ci.yml)

It scans a folder and produces:
- a score (0–100)
- a list of checks (info / warn / error) with suggested fixes
- optional baseline comparison (delta between runs)
- exportable reports (Markdown) for sharing

## Installation

Download the latest build:

- Windows: https://github.com/skunpro/repo-health/releases/latest/download/repo-health-windows.exe
- Linux: https://github.com/skunpro/repo-health/releases/latest/download/repo-health-linux
- macOS: https://github.com/skunpro/repo-health/releases/latest/download/repo-health-macos

Quick download:

[![Download Windows](https://img.shields.io/badge/download-windows-2ea043?style=for-the-badge)](https://github.com/skunpro/repo-health/releases/latest/download/repo-health-windows.exe)
[![Download Linux](https://img.shields.io/badge/download-linux-2ea043?style=for-the-badge)](https://github.com/skunpro/repo-health/releases/latest/download/repo-health-linux)
[![Download macOS](https://img.shields.io/badge/download-macos-2ea043?style=for-the-badge)](https://github.com/skunpro/repo-health/releases/latest/download/repo-health-macos)

Or open the Releases page: https://github.com/skunpro/repo-health/releases/latest

### Verify downloads (recommended)

Windows (PowerShell):

```powershell
Get-FileHash .\repo-health-windows.exe -Algorithm SHA256
```

Or:

```powershell
CertUtil -hashfile .\repo-health-windows.exe SHA256
```

Compare it with the SHA256 shown next to the asset on the GitHub Release page.

## Quick start

### Windows (non-technical)

1. Download the latest Windows `.exe` from the links above.
2. Run it (double-click) or drag & drop a folder onto the `.exe`.
3. In the UI press `h` for help.

### CLI

Run TUI (default):

```bash
repo-health
repo-health path/to/repo
```

Pick a folder (system picker):

```bash
repo-health --pick
```

Print a report:

```bash
repo-health --mode pretty path/to/repo
repo-health --mode md path/to/repo --out repo-health-report.md
repo-health --mode json path/to/repo --out repo-health-report.json
```

Baseline:

```bash
repo-health --mode pretty path/to/repo --save-baseline
repo-health --mode pretty path/to/repo
```

CI mode (non-zero exit code on warn/error):

```bash
repo-health --mode pretty path/to/repo --fail-on warn
repo-health --mode pretty path/to/repo --fail-on error
```

## Controls (TUI)

- Navigation: ↑/↓ or j/k (select), PgUp/PgDn (scroll details)
- Actions: o (open folder), r (rescan), b (save baseline), e (export report), h/? (help)
- Exit: q / Esc

## Contributing

See CONTRIBUTING.md.

## Contributors

| Contributor | Contact |
| --- | --- |
| <img src="https://github.com/skunpro.png?size=64" width="64" height="64" style="border-radius: 9999px;" alt="skun" /> | skun (<hello@skun.pro>) |

## License

MIT
