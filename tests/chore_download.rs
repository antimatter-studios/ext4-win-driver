//! The chore download retries a transient server error.
//!
//! GitHub's release download answers an occasional HTTP 500, and a `curl`
//! with no retry turns that one answer into a red job before any of the
//! change under test has run. Every workflow that downloads a chore release
//! must retry it at least three times, and on any error. These checks read
//! the workflows as text; they need no Windows.

use std::fs;
use std::path::PathBuf;

/// Every `curl` command in a workflow, comments dropped and `\`
/// continuations joined, so a flag on the next line still counts.
fn curl_commands(text: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut joined = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let continued = line.ends_with('\\');
        joined.push_str(line.trim_end_matches('\\'));
        joined.push(' ');
        if continued {
            continue;
        }
        if joined
            .split(|c: char| c.is_whitespace() || c == '(' || c == '|')
            .any(|word| word == "curl")
        {
            commands.push(joined.trim().to_string());
        }
        joined.clear();
    }
    commands
}

/// Whether a `curl` command survives one transient server error: it retries
/// at least three times, and on any error, not only on a refused connection.
fn retries_a_transient_error(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    let retries = words
        .windows(2)
        .find(|pair| pair[0] == "--retry")
        .and_then(|pair| pair[1].parse::<u32>().ok())
        .unwrap_or(0);
    retries >= 3 && words.contains(&"--retry-all-errors")
}

#[test]
fn a_retried_download_is_told_from_a_bare_one() {
    let bare = "curl -fsSL \"https://github.com/antimatter-studios/chore/releases/download/v1/a\" \\\n  | tar xz";
    let retried = "curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 \\\n  \"$url\" | tar xz";
    let too_few = "curl -fsSL --retry 1 --retry-all-errors \"$url\"";
    let commands = curl_commands(&format!(
        "{bare}\n# curl in a comment\n{retried}\n{too_few}\n"
    ));
    assert_eq!(commands.len(), 3, "{commands:#?}");
    assert!(!retries_a_transient_error(&commands[0]));
    assert!(retries_a_transient_error(&commands[1]));
    assert!(!retries_a_transient_error(&commands[2]));
}

#[test]
fn the_chore_download_retries_a_transient_server_error() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut downloads = 0;
    let mut bare = Vec::new();
    for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("a workflow").path();
        let text =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for command in curl_commands(&text) {
            if !command.contains("chore/releases/download") {
                continue;
            }
            downloads += 1;
            if !retries_a_transient_error(&command) {
                bare.push(format!("{}: {command}", path.display()));
            }
        }
    }
    assert!(
        downloads > 0,
        "found no chore download to check; the guard is looking in the wrong place"
    );
    assert!(
        bare.is_empty(),
        "these chore downloads fail the job on one transient HTTP 5xx; \
         add `--retry 5 --retry-all-errors --retry-delay 2`: {bare:#?}"
    );
}
