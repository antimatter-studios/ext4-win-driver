//! The WinFsp install fails its own step when WinFsp is not installed.
//!
//! `choco install winfsp` exits 0 having installed nothing when the
//! Chocolatey feed cannot serve the package ("installed 0/0 packages"). The
//! job then fails much later, in `winfsp-sys`'s build script, with "WinFsp
//! installation directory not found", which reads like a code problem
//! (#49). Every workflow therefore installs WinFsp through
//! `scripts/install-winfsp.ps1`, which retries a feed error and then refuses
//! to finish unless WinFsp's headers are on disk. These checks read the
//! workflows and the script as text; they need no Windows.

use std::fs;
use std::path::{Path, PathBuf};

const SCRIPT: &str = "scripts/install-winfsp.ps1";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every workflow and composite action, as (path, text).
fn workflow_files() -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    let mut dirs = vec![root().join(".github")];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("yml" | "yaml")
            ) {
                let text = fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                files.push((path, text));
            }
        }
    }
    files
}

/// Lines that run `choco install winfsp`, comments dropped.
fn bare_installs(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.trim_start_matches("run:").trim())
        .filter(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words
                .windows(3)
                .any(|w| w == ["choco", "install", "winfsp"])
        })
        .map(str::to_string)
        .collect()
}

fn relative(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap_or(path)
        .display()
        .to_string()
}

#[test]
fn a_bare_install_is_told_from_a_scripted_one() {
    let text = "run: choco install winfsp --yes --no-progress\n\
                # choco install winfsp in a comment\n\
                run: pwsh -File scripts/install-winfsp.ps1\n";
    assert_eq!(bare_installs(text).len(), 1);
}

#[test]
fn no_workflow_installs_winfsp_without_the_script() {
    let mut bare = Vec::new();
    let mut scripted = 0;
    for (path, text) in workflow_files() {
        for line in bare_installs(&text) {
            bare.push(format!("{}: {line}", relative(&path)));
        }
        scripted += text.matches(SCRIPT).count();
    }
    assert!(
        bare.is_empty(),
        "these steps run `choco install winfsp` directly, which passes having installed \
         nothing; run {SCRIPT} instead:\n{}",
        bare.join("\n")
    );
    assert!(
        scripted > 0,
        "no workflow runs {SCRIPT}, so nothing installs WinFsp at all"
    );
}

#[test]
fn the_script_retries_and_then_checks_the_install() {
    let path = root().join(SCRIPT);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let code: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        code.contains("choco install winfsp"),
        "{SCRIPT} never installs WinFsp"
    );
    assert!(
        code.contains("$LASTEXITCODE"),
        "{SCRIPT} ignores choco's exit status"
    );
    let attempts = code
        .lines()
        .find_map(|line| line.trim().strip_prefix("$attempts = "))
        .and_then(|n| n.trim().parse::<u32>().ok())
        .unwrap_or(0);
    assert!(
        attempts >= 3,
        "{SCRIPT} must try at least three times, found {attempts}"
    );
    assert!(
        code.contains("winfsp.h") && code.contains("Test-Path"),
        "{SCRIPT} must check WinFsp's headers are on disk before the build starts"
    );
    assert!(
        code.contains("exit 1"),
        "{SCRIPT} must fail its step when WinFsp is missing"
    );
}
