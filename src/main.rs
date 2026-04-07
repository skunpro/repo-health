use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, ValueEnum};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use rfd::FileDialog;
use serde::{Deserialize, Serialize};
use walkdir::{DirEntry, WalkDir};

#[derive(Parser)]
#[command(name = "repo-health", version, about = "Repo health scanner with TUI")]
struct Cli {
    #[arg(default_value = ".")]
    path: PathBuf,
    #[arg(long, default_value_t = false)]
    pick: bool,
    #[arg(long, value_enum, default_value = "tui")]
    mode: Mode,
    #[arg(long, default_value_t = 80_000)]
    max_files: usize,
    #[arg(long, value_enum, default_value = "off")]
    fail_on: FailOn,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    baseline: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    save_baseline: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Mode {
    Tui,
    Pretty,
    Json,
    Md,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum FailOn {
    Off,
    Warn,
    Error,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileEntry {
    path: String,
    bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RepoMetrics {
    root: String,
    total_files: usize,
    total_bytes: u64,
    largest_files: Vec<FileEntry>,
    detected: Vec<String>,
    score: u8,
    counts: SeverityCounts,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct SeverityCounts {
    info: usize,
    warn: usize,
    error: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CheckResult {
    id: String,
    title: String,
    severity: Severity,
    message: String,
    details: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ScanResult {
    metrics: RepoMetrics,
    checks: Vec<CheckResult>,
}

#[derive(Clone, Debug)]
struct BaselineDiff {
    baseline_score: u8,
    score_delta: i32,
    warn_delta: i32,
    error_delta: i32,
    regressions: Vec<String>,
}

#[derive(Clone, Debug)]
struct ScanOptions {
    max_files: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut root = cli.path.clone();
    if cli.pick {
        if let Some(picked) = FileDialog::new().pick_folder() {
            root = picked;
        } else {
            return Ok(());
        }
    }
    let root = root.canonicalize().unwrap_or(root);
    let opts = ScanOptions {
        max_files: cli.max_files,
    };

    match cli.mode {
        Mode::Tui => {
            let export_path = cli
                .out
                .unwrap_or_else(|| PathBuf::from("repo-health-report.md"));
            let baseline_path = cli
                .baseline
                .unwrap_or_else(|| PathBuf::from("repo-health-baseline.json"));
            run_tui(&root, &opts, export_path, baseline_path)
        }
        Mode::Pretty | Mode::Json | Mode::Md => {
            let result = scan_repo(&root, &opts)?;
            if cli.save_baseline {
                let baseline_path = cli
                    .baseline
                    .unwrap_or_else(|| PathBuf::from("repo-health-baseline.json"));
                save_baseline(&result, &root, &baseline_path)?;
                return Ok(());
            }

            let baseline = resolve_baseline(&root, cli.baseline.as_deref());
            let baseline_diff = baseline.as_ref().map(|b| compute_baseline_diff(&result, b));
            if let Some(out_path) = cli.out.as_deref() {
                write_report_to_path(out_path, cli.mode, &result, baseline_diff.as_ref())?;
            } else {
                write_report_to_stdout(cli.mode, &result, baseline_diff.as_ref())?;
            }
            if should_fail(&result.checks, cli.fail_on) {
                process::exit(2);
            }
            Ok(())
        }
    }
}

fn write_report_to_stdout(
    mode: Mode,
    result: &ScanResult,
    baseline_diff: Option<&BaselineDiff>,
) -> Result<()> {
    let mut out = io::stdout().lock();
    write_report(&mut out, mode, result, baseline_diff)?;
    writeln!(out)?;
    Ok(())
}

fn write_report_to_path(
    path: &Path,
    mode: Mode,
    result: &ScanResult,
    baseline_diff: Option<&BaselineDiff>,
) -> Result<()> {
    let mut out = io::BufWriter::new(fs::File::create(path)?);
    write_report(&mut out, mode, result, baseline_diff)?;
    out.flush()?;
    Ok(())
}

fn write_report<W: Write>(
    out: &mut W,
    mode: Mode,
    result: &ScanResult,
    baseline_diff: Option<&BaselineDiff>,
) -> Result<()> {
    match mode {
        Mode::Tui => Ok(()),
        Mode::Pretty => write_pretty(out, result, baseline_diff),
        Mode::Json => write_json(out, result),
        Mode::Md => write_markdown(out, result, baseline_diff),
    }
}

fn write_json<W: Write>(out: &mut W, result: &ScanResult) -> Result<()> {
    serde_json::to_writer_pretty(out, result)?;
    Ok(())
}

fn write_pretty<W: Write>(
    out: &mut W,
    result: &ScanResult,
    baseline_diff: Option<&BaselineDiff>,
) -> Result<()> {
    let m = &result.metrics;
    writeln!(out, "repo-health")?;
    writeln!(out, "root: {}", m.root)?;
    writeln!(
        out,
        "files: {} | size: {}",
        m.total_files,
        format_bytes(m.total_bytes)
    )?;
    writeln!(
        out,
        "score: {} | checks: {} ({} info, {} warn, {} error)",
        m.score,
        m.counts.info + m.counts.warn + m.counts.error,
        m.counts.info,
        m.counts.warn,
        m.counts.error
    )?;
    if let Some(d) = baseline_diff {
        writeln!(
            out,
            "baseline: score {} (delta {:+}), warn delta {:+}, error delta {:+}",
            d.baseline_score, d.score_delta, d.warn_delta, d.error_delta
        )?;
        if !d.regressions.is_empty() {
            for r in d.regressions.iter().take(8) {
                writeln!(out, "  regression: {}", r)?;
            }
            if d.regressions.len() > 8 {
                writeln!(out, "  regression: …")?;
            }
        }
    }
    if !m.detected.is_empty() {
        writeln!(out, "detected: {}", m.detected.join(", "))?;
    }
    if !m.largest_files.is_empty() {
        writeln!(out)?;
        writeln!(out, "largest files:")?;
        for f in &m.largest_files {
            writeln!(out, "  {:>10}  {}", format_bytes(f.bytes), f.path)?;
        }
    }
    writeln!(out)?;
    writeln!(out, "checks:")?;
    for c in &result.checks {
        writeln!(
            out,
            "  [{}] {} - {}",
            match c.severity {
                Severity::Info => "info",
                Severity::Warn => "warn",
                Severity::Error => "error",
            },
            c.title,
            c.message
        )?;
        for d in &c.details {
            writeln!(out, "       - {}", d)?;
        }
    }
    Ok(())
}

fn write_markdown<W: Write>(
    out: &mut W,
    result: &ScanResult,
    baseline_diff: Option<&BaselineDiff>,
) -> Result<()> {
    let m = &result.metrics;
    writeln!(out, "# repo-health")?;
    writeln!(out)?;
    writeln!(out, "**Root:** `{}`", m.root)?;
    writeln!(out)?;
    writeln!(out, "| Metric | Value |")?;
    writeln!(out, "|---|---:|")?;
    writeln!(out, "| Score | {} |", m.score)?;
    writeln!(out, "| Files | {} |", m.total_files)?;
    writeln!(out, "| Size | {} |", format_bytes(m.total_bytes))?;
    writeln!(
        out,
        "| Checks | {} ({} info, {} warn, {} error) |",
        m.counts.info + m.counts.warn + m.counts.error,
        m.counts.info,
        m.counts.warn,
        m.counts.error
    )?;
    if !m.detected.is_empty() {
        writeln!(out, "| Detected | {} |", m.detected.join(", "))?;
    }
    if let Some(d) = baseline_diff {
        writeln!(
            out,
            "| Baseline | score {} (delta {:+}), warn delta {:+}, error delta {:+} |",
            d.baseline_score, d.score_delta, d.warn_delta, d.error_delta
        )?;
    }

    if !m.largest_files.is_empty() {
        writeln!(out)?;
        writeln!(out, "## Largest files")?;
        writeln!(out)?;
        writeln!(out, "| Size | Path |")?;
        writeln!(out, "|---:|---|")?;
        for f in &m.largest_files {
            writeln!(out, "| {} | `{}` |", format_bytes(f.bytes), f.path)?;
        }
    }

    writeln!(out)?;
    writeln!(out, "## Checks")?;
    writeln!(out)?;
    for c in &result.checks {
        let sev = match c.severity {
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
        };
        writeln!(out, "- **[{}] {}** - {}", sev, c.title, c.message)?;
        for d in &c.details {
            writeln!(out, "  - {}", d)?;
        }
    }
    Ok(())
}

fn should_fail(checks: &[CheckResult], fail_on: FailOn) -> bool {
    let threshold = match fail_on {
        FailOn::Off => return false,
        FailOn::Warn => Severity::Warn,
        FailOn::Error => Severity::Error,
    };
    checks.iter().any(|c| severity_ge(c.severity, threshold))
}

fn resolve_baseline(root: &Path, explicit: Option<&Path>) -> Option<ScanResult> {
    if let Some(p) = explicit {
        return load_baseline(root, p);
    }
    let default_path = root.join("repo-health-baseline.json");
    if default_path.exists() {
        return load_baseline(root, Path::new("repo-health-baseline.json"));
    }
    None
}

fn load_baseline(root: &Path, baseline_path: &Path) -> Option<ScanResult> {
    let p = if baseline_path.is_absolute() {
        baseline_path.to_path_buf()
    } else {
        root.join(baseline_path)
    };
    let raw = fs::read_to_string(p).ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_baseline(result: &ScanResult, root: &Path, baseline_path: &Path) -> Result<()> {
    let p = if baseline_path.is_absolute() {
        baseline_path.to_path_buf()
    } else {
        root.join(baseline_path)
    };
    let out = serde_json::to_string_pretty(result)?;
    fs::write(&p, out)?;
    Ok(())
}

fn compute_baseline_diff(current: &ScanResult, baseline: &ScanResult) -> BaselineDiff {
    use std::collections::HashMap;

    let mut base = HashMap::<&str, Severity>::new();
    for c in &baseline.checks {
        base.insert(&c.id, c.severity);
    }

    let mut regressions = Vec::new();
    for c in &current.checks {
        let Some(prev_sev) = base.get(c.id.as_str()).copied() else {
            if c.severity != Severity::Info {
                regressions.push(format!("new {}: {}", sev_tag(c.severity), c.title));
            }
            continue;
        };
        if severity_rank(c.severity) > severity_rank(prev_sev) {
            regressions.push(format!(
                "{}: {} -> {}",
                c.title,
                sev_tag(prev_sev),
                sev_tag(c.severity)
            ));
        }
    }

    let score_delta = current.metrics.score as i32 - baseline.metrics.score as i32;
    let warn_delta = current.metrics.counts.warn as i32 - baseline.metrics.counts.warn as i32;
    let error_delta = current.metrics.counts.error as i32 - baseline.metrics.counts.error as i32;

    BaselineDiff {
        baseline_score: baseline.metrics.score,
        score_delta,
        warn_delta,
        error_delta,
        regressions,
    }
}

fn sev_tag(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "error",
    }
}

fn severity_ge(a: Severity, b: Severity) -> bool {
    severity_rank(a) >= severity_rank(b)
}

fn severity_rank(s: Severity) -> u8 {
    match s {
        Severity::Info => 0,
        Severity::Warn => 1,
        Severity::Error => 2,
    }
}

fn scan_repo(root: &Path, opts: &ScanOptions) -> Result<ScanResult> {
    let root = root
        .canonicalize()
        .with_context(|| format!("Cannot resolve path: {}", root.display()))?;
    if !root.is_dir() {
        return Err(anyhow!("Not a directory: {}", root.display()));
    }

    let mut total_files = 0usize;
    let mut total_bytes = 0u64;
    let mut largest: Vec<FileEntry> = Vec::new();

    let mut flags = DetectedFlags::default();
    let mut saw_env_file = false;

    let walker = WalkDir::new(&root).follow_links(false).into_iter();
    for entry in walker.filter_entry(should_descend) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        total_files += 1;
        if total_files > opts.max_files {
            break;
        }

        if let Some(name) = entry.file_name().to_str() {
            if name.eq_ignore_ascii_case("package.json") {
                flags.node = true;
            } else if name.eq_ignore_ascii_case("Cargo.toml") {
                flags.rust = true;
            } else if name.eq_ignore_ascii_case("pyproject.toml")
                || name.eq_ignore_ascii_case("requirements.txt")
            {
                flags.python = true;
            } else if name.eq_ignore_ascii_case(".env") || name.ends_with(".env") {
                saw_env_file = true;
            }
        }

        let md = match entry.metadata() {
            Ok(md) => md,
            Err(_) => continue,
        };
        let size = md.len();
        total_bytes = total_bytes.saturating_add(size);
        maybe_push_largest(
            &mut largest,
            FileEntry {
                path: rel_path(&root, entry.path()),
                bytes: size,
            },
            10,
        );
    }

    largest.sort_by_key(|f| Reverse(f.bytes));

    let mut checks = Vec::new();
    checks.push(check_gitignore_env(&root, saw_env_file));
    checks.push(check_large_files(&largest));
    if flags.node {
        checks.push(check_node_scripts(&root));
    }
    if flags.python {
        checks.push(check_python_tooling(&root));
    }
    checks.push(check_secret_markers(&root, opts.max_files));

    let detected = flags.as_vec();
    let counts = count_severities(&checks);
    let score = compute_score(&counts);
    let metrics = RepoMetrics {
        root: root.display().to_string(),
        total_files,
        total_bytes,
        largest_files: largest.clone(),
        detected,
        score,
        counts,
    };

    Ok(ScanResult { metrics, checks })
}

fn count_severities(checks: &[CheckResult]) -> SeverityCounts {
    let mut c = SeverityCounts::default();
    for r in checks {
        match r.severity {
            Severity::Info => c.info += 1,
            Severity::Warn => c.warn += 1,
            Severity::Error => c.error += 1,
        }
    }
    c
}

fn compute_score(counts: &SeverityCounts) -> u8 {
    let mut score: i32 = 100;
    score -= (counts.warn as i32) * 10;
    score -= (counts.error as i32) * 25;
    score.clamp(0, 100) as u8
}

#[derive(Default, Clone, Copy)]
struct DetectedFlags {
    node: bool,
    rust: bool,
    python: bool,
}

impl DetectedFlags {
    fn as_vec(self) -> Vec<String> {
        let mut v = Vec::new();
        if self.node {
            v.push("node".to_string());
        }
        if self.rust {
            v.push("rust".to_string());
        }
        if self.python {
            v.push("python".to_string());
        }
        v
    }
}

fn check_gitignore_env(root: &Path, saw_env_file: bool) -> CheckResult {
    let mut details = Vec::new();
    let gitignore = root.join(".gitignore");
    let ignored = match fs::read_to_string(&gitignore) {
        Ok(s) => gitignore_has_env_rule(&s),
        Err(_) => false,
    };

    if saw_env_file && !ignored {
        details.push(
            "Found .env-like files, but .gitignore does not include an .env rule.".to_string(),
        );
        return CheckResult {
            id: "gitignore_env".to_string(),
            title: ".env ignored".to_string(),
            severity: Severity::Warn,
            message: "Add .env to .gitignore to reduce secret leaks.".to_string(),
            details,
        };
    }

    let severity = if saw_env_file && ignored {
        Severity::Info
    } else if saw_env_file && !ignored {
        Severity::Warn
    } else {
        Severity::Info
    };

    let message = if saw_env_file {
        if ignored {
            "Env files detected and ignored.".to_string()
        } else {
            "Env files detected.".to_string()
        }
    } else if ignored {
        "No env files detected, but .gitignore contains env rules.".to_string()
    } else {
        "No env files detected.".to_string()
    };

    CheckResult {
        id: "gitignore_env".to_string(),
        title: ".env ignored".to_string(),
        severity,
        message,
        details,
    }
}

fn gitignore_has_env_rule(contents: &str) -> bool {
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == ".env" || line == "*.env" || line.ends_with("/.env") || line.ends_with("/*.env")
        {
            return true;
        }
    }
    false
}

fn check_node_scripts(root: &Path) -> CheckResult {
    let path = root.join("package.json");
    let mut details = Vec::new();

    let value: serde_json::Value = match fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(v) => v,
        None => {
            return CheckResult {
                id: "node_scripts".to_string(),
                title: "Node scripts".to_string(),
                severity: Severity::Warn,
                message: "package.json could not be parsed.".to_string(),
                details,
            };
        }
    };

    let scripts = value
        .get("scripts")
        .and_then(|s| s.as_object())
        .cloned()
        .unwrap_or_default();

    let has_lint = scripts.contains_key("lint");
    let has_build = scripts.contains_key("build");
    let has_typecheck = scripts.contains_key("typecheck")
        || scripts.contains_key("check-types")
        || scripts.contains_key("tsc");

    if !has_lint {
        details.push("Missing script: lint".to_string());
    }
    if !has_build {
        details.push("Missing script: build".to_string());
    }
    if !has_typecheck {
        details.push("Missing script: typecheck (or check-types/tsc)".to_string());
    }

    let severity = if details.is_empty() {
        Severity::Info
    } else {
        Severity::Warn
    };

    let message = if details.is_empty() {
        "Found lint/build/typecheck scripts.".to_string()
    } else {
        "Add missing scripts for consistent CI.".to_string()
    };

    CheckResult {
        id: "node_scripts".to_string(),
        title: "Node scripts".to_string(),
        severity,
        message,
        details,
    }
}

fn check_python_tooling(root: &Path) -> CheckResult {
    let mut details = Vec::new();
    let mut found = BTreeMap::<&str, bool>::new();
    found.insert("ruff", false);
    found.insert("black", false);
    found.insert("mypy", false);
    found.insert("pytest", false);

    let req = root.join("requirements.txt");
    if let Ok(s) = fs::read_to_string(&req) {
        for (k, v) in found.iter_mut() {
            if s.to_lowercase().contains(k) {
                *v = true;
            }
        }
    }

    let pyproject = root.join("pyproject.toml");
    if let Ok(s) = fs::read_to_string(&pyproject) {
        let lower = s.to_lowercase();
        if lower.contains("[tool.ruff]") {
            found.insert("ruff", true);
        }
        if lower.contains("[tool.black]") {
            found.insert("black", true);
        }
        if lower.contains("[tool.mypy]") {
            found.insert("mypy", true);
        }
        if lower.contains("[tool.pytest]") || lower.contains("[tool.pytest.ini_options]") {
            found.insert("pytest", true);
        }
    }

    for (k, v) in &found {
        if !*v {
            details.push(format!("Missing: {}", k));
        }
    }

    let severity = if details.len() == found.len() {
        Severity::Warn
    } else if details.is_empty() {
        Severity::Info
    } else {
        Severity::Warn
    };

    let message = if details.is_empty() {
        "Python tooling detected.".to_string()
    } else {
        "Consider adding lint/format/typecheck/test tooling.".to_string()
    };

    CheckResult {
        id: "python_tooling".to_string(),
        title: "Python tooling".to_string(),
        severity,
        message,
        details,
    }
}

fn check_large_files(largest: &[FileEntry]) -> CheckResult {
    let mut details = Vec::new();
    for f in largest {
        if f.bytes >= 50 * 1024 * 1024 {
            details.push(format!("{} ({})", f.path, format_bytes(f.bytes)));
        }
    }

    let severity = if details.is_empty() {
        Severity::Info
    } else {
        Severity::Warn
    };

    let message = if details.is_empty() {
        "No very large files in top list.".to_string()
    } else {
        "Very large files detected (>= 50MB).".to_string()
    };

    CheckResult {
        id: "large_files".to_string(),
        title: "Large files".to_string(),
        severity,
        message,
        details,
    }
}

fn check_secret_markers(root: &Path, max_files: usize) -> CheckResult {
    let mut details = Vec::new();
    let patterns: [(&str, &str); 5] = [
        ("aws_access_key_id", r"AKIA[0-9A-Z]{16}"),
        ("private_key", r"-----BEGIN (?:RSA )?PRIVATE KEY-----"),
        ("github_pat", r"ghp_[A-Za-z0-9]{36,}"),
        (
            "discord_token",
            r"(?i)(discord|bot)[-_ ]?token\s*=\s*[^\s]{20,}",
        ),
        ("stripe_key", r"sk_(?:live|test)_[A-Za-z0-9]{16,}"),
    ];
    let compiled: Vec<(String, regex::Regex)> = patterns
        .iter()
        .filter_map(|(name, pat)| {
            regex::Regex::new(pat)
                .ok()
                .map(|re| ((*name).to_string(), re))
        })
        .collect();

    let mut scanned = 0usize;
    let walker = WalkDir::new(root).follow_links(false).into_iter();
    for entry in walker.filter_entry(should_descend) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        scanned += 1;
        if scanned > max_files.min(15_000) {
            break;
        }

        let p = entry.path();
        if is_likely_binary_path(p) {
            continue;
        }
        let md = match entry.metadata() {
            Ok(md) => md,
            Err(_) => continue,
        };
        if md.len() > 256 * 1024 {
            continue;
        }
        let Ok(content) = fs::read_to_string(p) else {
            continue;
        };
        for (name, re) in &compiled {
            if re.is_match(&content) {
                details.push(format!("{}: {}", name, rel_path(root, p)));
                if details.len() >= 25 {
                    break;
                }
            }
        }
        if details.len() >= 25 {
            break;
        }
    }

    let severity = if details.is_empty() {
        Severity::Info
    } else {
        Severity::Error
    };

    let message = if details.is_empty() {
        "No obvious secret markers found (light scan).".to_string()
    } else {
        "Potential secret markers found.".to_string()
    };

    CheckResult {
        id: "secrets".to_string(),
        title: "Secrets".to_string(),
        severity,
        message,
        details,
    }
}

fn is_likely_binary_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let name_l = name.to_lowercase();
    if name_l.ends_with(".png")
        || name_l.ends_with(".jpg")
        || name_l.ends_with(".jpeg")
        || name_l.ends_with(".webp")
        || name_l.ends_with(".gif")
        || name_l.ends_with(".mp4")
        || name_l.ends_with(".mp3")
        || name_l.ends_with(".wav")
        || name_l.ends_with(".ico")
        || name_l.ends_with(".pdf")
        || name_l.ends_with(".zip")
        || name_l.ends_with(".rar")
        || name_l.ends_with(".7z")
        || name_l.ends_with(".exe")
        || name_l.ends_with(".dll")
        || name_l.ends_with(".node")
    {
        return true;
    }
    false
}

fn should_descend(entry: &DirEntry) -> bool {
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    if entry.depth() == 0 {
        return true;
    }
    !matches!(
        name,
        ".git"
            | "target"
            | "node_modules"
            | "__pycache__"
            | ".pytest_cache"
            | "dist"
            | "build"
            | ".next"
            | ".turbo"
            | ".venv"
            | "venv"
    )
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn maybe_push_largest(list: &mut Vec<FileEntry>, entry: FileEntry, cap: usize) {
    list.push(entry);
    list.sort_by_key(|f| Reverse(f.bytes));
    list.truncate(cap);
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.2} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

fn run_tui(
    path: &Path,
    opts: &ScanOptions,
    export_path: PathBuf,
    baseline_path: PathBuf,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut app = AppState::new(path.to_path_buf(), opts.clone(), export_path, baseline_path)?;

    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| {
                let size = f.area();
                draw_ui(f, size, &mut app);
            })?;

            if event::poll(Duration::from_millis(150))? {
                match event::read()? {
                    Event::Key(k) => {
                        if handle_key(&mut app, k)? {
                            break;
                        }
                    }
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

#[derive(Clone)]
struct AppState {
    root: PathBuf,
    opts: ScanOptions,
    last_scan_ms: u128,
    result: Option<ScanResult>,
    baseline: Option<ScanResult>,
    baseline_diff: Option<BaselineDiff>,
    status: String,
    selected_check: usize,
    detail_scroll: u16,
    export_path: PathBuf,
    baseline_path: PathBuf,
    show_help: bool,
}

impl AppState {
    fn new(
        root: PathBuf,
        opts: ScanOptions,
        export_path: PathBuf,
        baseline_path: PathBuf,
    ) -> Result<Self> {
        let root = root.canonicalize().unwrap_or(root);
        let mut s = Self {
            root,
            opts,
            last_scan_ms: 0,
            result: None,
            baseline: None,
            baseline_diff: None,
            status: String::new(),
            selected_check: 0,
            detail_scroll: 0,
            export_path,
            baseline_path,
            show_help: false,
        };
        s.reload_baseline();
        s.rescan()?;
        Ok(s)
    }

    fn rescan(&mut self) -> Result<()> {
        self.status = "Scanning…".to_string();
        let start = now_ms();
        match scan_repo(&self.root, &self.opts) {
            Ok(r) => {
                let max = r.checks.len().saturating_sub(1);
                self.selected_check = self.selected_check.min(max);
                self.detail_scroll = 0;
                self.baseline_diff = self.baseline.as_ref().map(|b| compute_baseline_diff(&r, b));
                self.result = Some(r);
                self.status = "OK".to_string();
            }
            Err(e) => {
                self.result = None;
                self.status = format!("Error: {}", e);
            }
        }
        self.last_scan_ms = now_ms().saturating_sub(start);
        Ok(())
    }

    fn checks_len(&self) -> usize {
        self.result.as_ref().map(|r| r.checks.len()).unwrap_or(0)
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.checks_len();
        if len == 0 {
            self.selected_check = 0;
            return;
        }
        let cur = self.selected_check as i32;
        let next = (cur + delta).clamp(0, (len - 1) as i32);
        if next as usize != self.selected_check {
            self.selected_check = next as usize;
            self.detail_scroll = 0;
        }
    }

    fn scroll_detail(&mut self, delta: i16) {
        let next = (self.detail_scroll as i16 + delta).max(0) as u16;
        self.detail_scroll = next;
    }

    fn export_markdown(&mut self) -> Result<()> {
        let Some(result) = &self.result else {
            self.status = "Nothing to export.".to_string();
            return Ok(());
        };

        let path = if self.export_path.is_absolute() {
            self.export_path.clone()
        } else {
            self.root.join(&self.export_path)
        };

        let mut out = io::BufWriter::new(fs::File::create(&path)?);
        write_markdown(&mut out, result, self.baseline_diff.as_ref())?;
        out.flush()?;
        self.status = format!("Exported: {}", path.display());
        Ok(())
    }

    fn reload_baseline(&mut self) {
        self.baseline = load_baseline(&self.root, &self.baseline_path);
    }

    fn save_baseline_current(&mut self) -> Result<()> {
        let Some(result) = &self.result else {
            self.status = "Nothing to baseline.".to_string();
            return Ok(());
        };
        save_baseline(result, &self.root, &self.baseline_path)?;
        self.baseline = Some(result.clone());
        self.baseline_diff = Some(compute_baseline_diff(result, result));
        self.status = format!(
            "Baseline saved: {}",
            if self.baseline_path.is_absolute() {
                self.baseline_path.display().to_string()
            } else {
                self.root.join(&self.baseline_path).display().to_string()
            }
        );
        Ok(())
    }

    fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    fn pick_new_root(&mut self) -> Result<()> {
        let picked = FileDialog::new().set_directory(&self.root).pick_folder();
        let Some(picked) = picked else {
            self.status = "Open folder canceled.".to_string();
            return Ok(());
        };
        self.root = picked.canonicalize().unwrap_or(picked);
        self.selected_check = 0;
        self.detail_scroll = 0;
        self.reload_baseline();
        self.rescan()?;
        self.status = format!("Opened: {}", self.root.display());
        Ok(())
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn handle_key(app: &mut AppState, key: KeyEvent) -> Result<bool> {
    if app.show_help {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => Ok(true),
            (KeyCode::Esc, _) | (KeyCode::Char('h'), _) | (KeyCode::Char('?'), _) => {
                app.show_help = false;
                Ok(false)
            }
            _ => Ok(false),
        }
    } else {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => Ok(true),
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => Ok(true),
            (KeyCode::Char('r'), _) => {
                app.rescan()?;
                Ok(false)
            }
            (KeyCode::Char('o'), _) => {
                if let Err(e) = app.pick_new_root() {
                    app.status = format!("Open folder error: {}", e);
                }
                Ok(false)
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                app.move_selection(-1);
                Ok(false)
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                app.move_selection(1);
                Ok(false)
            }
            (KeyCode::PageUp, _) => {
                app.scroll_detail(-8);
                Ok(false)
            }
            (KeyCode::PageDown, _) => {
                app.scroll_detail(8);
                Ok(false)
            }
            (KeyCode::Char('h'), _) | (KeyCode::Char('?'), _) => {
                app.toggle_help();
                Ok(false)
            }
            (KeyCode::Char('e'), _) => {
                if let Err(e) = app.export_markdown() {
                    app.status = format!("Export error: {}", e);
                }
                Ok(false)
            }
            (KeyCode::Char('b'), _) => {
                if let Err(e) = app.save_baseline_current() {
                    app.status = format!("Baseline error: {}", e);
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }
}

fn draw_ui(f: &mut ratatui::Frame, area: Rect, app: &mut AppState) {
    let outer = Block::default().borders(Borders::NONE);
    f.render_widget(outer, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(area);

    let score = app.result.as_ref().map(|r| r.metrics.score).unwrap_or(0);
    let baseline_hint = app
        .baseline_diff
        .as_ref()
        .map(|d| format!("baseline {} (delta {:+})", d.baseline_score, d.score_delta));
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("repo-health", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::raw(app.root.display().to_string()),
        ]),
        Line::from(vec![
            Span::styled("↑/↓", Style::default().fg(Color::Yellow)),
            Span::raw(" select  "),
            Span::styled("o", Style::default().fg(Color::Yellow)),
            Span::raw(" open  "),
            Span::styled("r", Style::default().fg(Color::Yellow)),
            Span::raw(" rescan  "),
            Span::styled("h", Style::default().fg(Color::Yellow)),
            Span::raw(" help  "),
            Span::styled("e", Style::default().fg(Color::Yellow)),
            Span::raw(" export  "),
            Span::styled("b", Style::default().fg(Color::Yellow)),
            Span::raw(" baseline  "),
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(" quit  "),
            Span::styled(
                format!("score {}", score),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                baseline_hint.unwrap_or_else(|| "baseline none".to_string()),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ])
    .block(Block::default().borders(Borders::BOTTOM))
    .wrap(Wrap { trim: true });
    f.render_widget(header, chunks[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(chunks[1]);

    draw_metrics(f, body[0], app);
    draw_checks(f, body[1], app);

    let footer = Paragraph::new(Line::from(vec![
        Span::raw(app.status.clone()),
        Span::raw("  "),
        Span::styled(
            format!("scan {}ms", app.last_scan_ms),
            Style::default().fg(Color::DarkGray),
        ),
    ]))
    .block(Block::default().borders(Borders::TOP));
    f.render_widget(footer, chunks[2]);

    if app.show_help {
        draw_help_overlay(f, area);
    }
}

fn draw_help_overlay(f: &mut ratatui::Frame, area: Rect) {
    let width = area.width.saturating_sub(6).clamp(30, 86);
    let height = area.height.saturating_sub(6).clamp(10, 18);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let rect = Rect {
        x,
        y,
        width,
        height,
    };

    f.render_widget(Clear, rect);

    let lines = vec![
        Line::from(vec![
            Span::styled("Navigation:", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  ↑/↓ or j/k select check, PgUp/PgDn scroll details"),
        ]),
        Line::from(vec![
            Span::styled("Actions:", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  o open folder, r rescan, e export report, b save baseline"),
        ]),
        Line::from(vec![
            Span::styled("Exit:", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  q or Esc (Ctrl+C)"),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("Files:", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  repo-health-report.md, repo-health-baseline.json"),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("CLI:", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  --mode pretty|md|json  --out <file>  --save-baseline"),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("Tip:", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  Press h or ? to close this help."),
        ]),
    ];

    let widget = Paragraph::new(lines)
        .block(
            Block::default()
                .title("Help")
                .borders(Borders::ALL)
                .style(Style::default().bg(Color::Black).fg(Color::White)),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(widget, rect);
}

fn draw_metrics(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let mut lines: Vec<Line> = Vec::new();
    if let Some(r) = &app.result {
        let m = &r.metrics;
        lines.push(Line::from(vec![
            Span::styled("Score: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(m.score.to_string()),
            Span::raw("  "),
            Span::styled(
                format!(
                    "({} info, {} warn, {} error)",
                    m.counts.info, m.counts.warn, m.counts.error
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        if let Some(d) = app.baseline_diff.as_ref() {
            lines.push(Line::from(vec![
                Span::styled("Baseline: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(d.baseline_score.to_string()),
                Span::raw("  "),
                Span::styled(
                    format!(
                        "dScore {:+} | dWarn {:+} | dError {:+}",
                        d.score_delta, d.warn_delta, d.error_delta
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            if !d.regressions.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(
                        "Regressions: ",
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        d.regressions.len().to_string(),
                        Style::default().fg(Color::Yellow),
                    ),
                ]));
            }
        }
        lines.push(Line::from(vec![
            Span::styled("Files: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(m.total_files.to_string()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Size: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format_bytes(m.total_bytes)),
        ]));
        if !m.detected.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Detected: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(m.detected.join(", ")),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Largest",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        for fentry in &m.largest_files {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>9}", format_bytes(fentry.bytes)),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw("  "),
                Span::raw(fentry.path.clone()),
            ]));
        }
    } else {
        lines.push(Line::raw("No data."));
    }

    let widget = Paragraph::new(lines)
        .block(Block::default().title("Metrics").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    f.render_widget(widget, area);
}

fn draw_checks(f: &mut ratatui::Frame, area: Rect, app: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    draw_checks_list(f, chunks[0], app);
    draw_check_details(f, chunks[1], app);
}

fn draw_checks_list(f: &mut ratatui::Frame, area: Rect, app: &mut AppState) {
    let (items, len) = match &app.result {
        Some(r) => {
            let items: Vec<ListItem> = r
                .checks
                .iter()
                .map(|c| {
                    let (color, tag) = match c.severity {
                        Severity::Info => (Color::Green, "info"),
                        Severity::Warn => (Color::Yellow, "warn"),
                        Severity::Error => (Color::Red, "error"),
                    };
                    let line = Line::from(vec![
                        Span::styled(format!("[{}] ", tag), Style::default().fg(color)),
                        Span::styled(
                            c.title.clone(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                    ]);
                    ListItem::new(line)
                })
                .collect();
            (items, r.checks.len())
        }
        None => (vec![ListItem::new("No data.")], 0),
    };

    let mut state = ListState::default();
    if len > 0 {
        state.select(Some(app.selected_check.min(len.saturating_sub(1))));
    }

    let list = List::new(items)
        .block(Block::default().title("Checks").borders(Borders::ALL))
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_stateful_widget(list, area, &mut state);
}

fn fix_hints_for_check(id: &str) -> &'static [&'static str] {
    match id {
        "gitignore_env" => &[
            "Add `.env` (and optionally `*.env`) to `.gitignore`.",
            "If secrets were committed, rotate them and rewrite history if needed.",
        ],
        "node_scripts" => &[
            "Add scripts: `lint`, `build`, and `typecheck` (or `tsc`).",
            "Run them in CI for consistent quality gates.",
        ],
        "python_tooling" => &[
            "Add ruff (lint+format), pytest (tests), and optionally mypy (typecheck).",
            "Expose `lint`/`test` commands so CI can run them.",
        ],
        "large_files" => &[
            "Consider Git LFS for large binaries/assets.",
            "Move generated artifacts out of the repo (build outputs, caches).",
        ],
        "secrets" => &[
            "Treat matches as leaks until proven otherwise.",
            "Rotate keys/tokens and move them to env/secret manager.",
            "Remove secrets from git history if they were committed.",
        ],
        _ => &[],
    }
}

fn draw_check_details(f: &mut ratatui::Frame, area: Rect, app: &mut AppState) {
    let mut lines: Vec<Line> = Vec::new();
    if let Some(r) = &app.result {
        if let Some(c) = r.checks.get(app.selected_check) {
            let (color, tag) = match c.severity {
                Severity::Info => (Color::Green, "info"),
                Severity::Warn => (Color::Yellow, "warn"),
                Severity::Error => (Color::Red, "error"),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("[{}] ", tag), Style::default().fg(color)),
                Span::styled(
                    c.title.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::raw(""));
            lines.push(Line::raw(c.message.clone()));
            if !c.details.is_empty() {
                lines.push(Line::raw(""));
                for d in &c.details {
                    lines.push(Line::from(vec![
                        Span::styled("• ", Style::default().fg(Color::DarkGray)),
                        Span::raw(d.clone()),
                    ]));
                }
            }
            let hints = fix_hints_for_check(&c.id);
            if !hints.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "Suggested fixes",
                    Style::default().add_modifier(Modifier::BOLD),
                ));
                for h in hints {
                    lines.push(Line::from(vec![
                        Span::styled("• ", Style::default().fg(Color::DarkGray)),
                        Span::raw((*h).to_string()),
                    ]));
                }
            }
        } else {
            lines.push(Line::raw("No selection."));
        }
    } else {
        lines.push(Line::raw("No data."));
    }

    let widget = Paragraph::new(lines)
        .block(Block::default().title("Details").borders(Borders::ALL))
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0));
    f.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(prefix: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let n = format!("{}_{}_{}", prefix, std::process::id(), now_ms());
        p.push(n);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn detects_gitignore_env_rule() {
        let text = r#"
        node_modules
        .env
        "#;
        assert!(gitignore_has_env_rule(text));
        assert!(!gitignore_has_env_rule("node_modules\n"));
    }

    #[test]
    fn node_scripts_check_warns_on_missing() {
        let dir = tmp_dir("repo_health_test_pkg");
        fs::write(
            dir.join("package.json"),
            r#"{"name":"x","scripts":{"lint":"eslint .","build":"vite build"}}"#,
        )
        .unwrap();
        let c = check_node_scripts(&dir);
        assert_eq!(c.severity, Severity::Warn);
        fs::remove_dir_all(dir).unwrap();
    }
}
