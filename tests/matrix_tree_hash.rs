//! A `verify-tree` scenario pins the sha256 of `ext4 tree` stdout.
//!
//! The raw listing prints each entry's inode number, in on-disk
//! directory order. Both come from whichever kernel populated the image
//! through its loop mount, so a pin over the raw form checks the image
//! builder's kernel as much as the driver, and fails on any image the
//! original builder did not make (issue #30).
//!
//! `ext4 tree --canonical` drops the inode numbers and sorts each
//! directory by name, so its output depends only on the names, types
//! and shape of the tree. This reads the matrix as text and refuses a
//! sha256 pin over anything else.

const MATRIX: &str = include_str!("../test-matrix.json");

/// The `expect_args` line of every `verify-tree` step, with its line
/// number. Each step is written as `"op": "verify-tree"` followed by its
/// `"expect_args"` within the same object.
fn verify_tree_steps() -> Vec<(usize, &'static str)> {
    let lines: Vec<&str> = MATRIX.lines().collect();
    let mut steps = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !line.contains("\"op\": \"verify-tree\"") {
            continue;
        }
        let args = lines[i + 1..]
            .iter()
            .enumerate()
            .take_while(|(_, l)| !l.trim_start().starts_with('}'))
            .find(|(_, l)| l.contains("\"expect_args\""))
            .map(|(j, l)| (i + 2 + j, *l));
        steps.push(args.unwrap_or((i + 1, *line)));
    }
    steps
}

#[test]
fn no_tree_hash_is_pinned_over_inode_numbers() {
    let mut offenders = Vec::new();
    for (lineno, args) in verify_tree_steps() {
        if args.contains("--expect-stdout-sha256") && !args.contains("'--canonical'") {
            offenders.push(format!(
                "test-matrix.json:{lineno}: pins a sha256 of raw `ext4 tree`, whose inode \
                 numbers depend on the kernel that built the image; pass `'--' '--canonical'`"
            ));
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}

#[test]
fn basic_tree_hash_still_checks_a_tree() {
    let steps = verify_tree_steps();
    assert!(
        !steps.is_empty(),
        "test-matrix.json has no verify-tree step; basic-tree-hash should carry one"
    );
    assert!(
        steps
            .iter()
            .any(|(_, args)| args.contains("--expect-stdout-sha256")),
        "no verify-tree step pins a hash, so nothing checks the tree's shape"
    );
}
