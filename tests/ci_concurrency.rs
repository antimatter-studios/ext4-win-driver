//! A pull request's red run is never cancelled by the push that adds its fix.
//!
//! A pull request opens holding only its failing test, with a title that
//! starts "red:", and the CI run of that commit is its red evidence
//! (agent-core v8). The fix is pushed to the same branch after the title
//! loses "red:". `ci.yml` cancels a pull request's run when a newer push
//! arrives, so without an exception the fix push would cancel the red run,
//! often before it has failed. So while the title starts "red:", each push
//! is a group of its own commit and is never cancelled: keyed by the commit
//! and not only by the ref, because GitHub keeps one pending run per group
//! even with cancelling off, and a second push sharing the group would
//! replace a red run waiting for a runner. Every other pull request run
//! still cancels the one it supersedes.
//!
//! The workflow is read as text, so this runs on any host.

use std::fs;
use std::path::Path;

/// The lines of `ci.yml`'s top-level `concurrency:` block, unindented.
fn concurrency_block() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let mut lines = text.lines().skip_while(|l| *l != "concurrency:");
    assert!(
        lines.next().is_some(),
        "ci.yml has no top-level concurrency block"
    );
    lines
        .take_while(|l| l.starts_with(' ') || l.starts_with('\t'))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_red_pull_requests_run_is_never_cancelled() {
    let block = concurrency_block();
    assert!(
        block.contains(
            "cancel-in-progress: ${{ github.event_name == 'pull_request' && !startsWith(github.event.pull_request.title, 'red:') }}"
        ),
        "ci.yml must cancel only a pull request's superseded run, never a red: pull request's:\n{block}"
    );
}

#[test]
fn each_push_to_a_red_pull_request_is_a_group_of_its_own_commit() {
    let block = concurrency_block();
    assert!(
        block.contains(
            "startsWith(github.event.pull_request.title, 'red:') && format('-red-{0}', github.event.pull_request.head.sha) || ''"
        ),
        "ci.yml must give each push to a red: pull request a group of its own commit:\n{block}"
    );
}

#[test]
fn other_runs_keep_one_group_per_ref() {
    let block = concurrency_block();
    assert!(
        block.contains("group: ci-${{ github.ref }}"),
        "ci.yml must keep one group per ref, so a pull request's newer push still cancels its older run:\n{block}"
    );
}
