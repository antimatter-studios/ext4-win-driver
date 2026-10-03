//! `ext4 parts` prints each partition's kind as the short label that
//! winfsp-fs-skeleton's partition parser documents (`GPT:linux`,
//! `GPT:efi`, `GPT:msbasic`, `GPT:swap`), and only falls back to
//! `GPT:<GUID>` for a type GUID it has no name for.
//!
//! The scenarios in `test-matrix.json` only run on the Windows matrix,
//! never in CI, so an expectation written against the raw GUID of a
//! named type fails there and nowhere else. This reads the matrix as
//! text and refuses any expectation that spells a named type as its
//! GUID, because the CLI can never print it that way.

const MATRIX: &str = include_str!("../test-matrix.json");

/// The type GUIDs the skeleton maps to a short label, with that label.
/// Mirrors `classify_gpt_guid` in winfsp-fs-skeleton's `partition.rs`.
const NAMED_GPT_TYPES: &[(&str, &str)] = &[
    ("0FC63DAF-8483-4772-8E79-3D69D8477DE4", "GPT:linux"),
    ("C12A7328-F81F-11D2-BA4B-00A0C93EC93B", "GPT:efi"),
    ("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7", "GPT:msbasic"),
    ("0657FD6D-A4AB-43C4-84E5-0933C84B4F4F", "GPT:swap"),
];

#[test]
fn no_expectation_spells_a_named_gpt_type_as_its_guid() {
    let mut offenders = Vec::new();
    for (lineno, line) in MATRIX.lines().enumerate() {
        let upper = line.to_ascii_uppercase();
        for (guid, label) in NAMED_GPT_TYPES {
            if upper.contains(&format!("GPT:{guid}")) {
                offenders.push(format!(
                    "test-matrix.json:{}: expects GPT:{guid}, but `ext4 parts` prints {label}",
                    lineno + 1
                ));
            }
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}

#[test]
fn whole_disk_parts_expects_the_linux_label() {
    let start = MATRIX
        .find("\"whole-disk-parts\"")
        .expect("test-matrix.json has no whole-disk-parts scenario");
    let rest = &MATRIX[start..];
    // Scenarios sit at four spaces of indent, so the first line that
    // closes at that indent closes this one.
    let end = rest.find("\n    }").unwrap_or(rest.len());
    let body = &rest[..end];
    assert!(
        body.contains("GPT:linux"),
        "whole-disk-parts must expect the documented `GPT:linux` label:\n{body}"
    );
}
