# Repo health scanner with a terminal UI.

It scans a folder and produces:
- a score (0–100)
- a list of checks (info / warn / error) with suggested fixes
- optional baseline comparison (delta between runs)
- exportable reports (Markdown) for sharing

## Installation

Recommended: download the `repo-health` binary from GitHub Releases.

## Quick start

### Windows (non-technical)

1. Download `repo-health.exe` from GitHub Releases.
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

## Contributors

| Contributor | Contact |
| --- | --- |
| <img src="https://github.com/skunpro.png?size=64" width="64" height="64" style="border-radius: 9999px;" alt="skun" /> | skun (<hello@skun.pro>) |

## License

MIT
