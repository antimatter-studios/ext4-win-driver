//! The matrix's ext4 images come from rust-fs-ext4's image builder, which
//! that repository removed at v0.6.0 (#37). The driver is pinned past that
//! (`FS_EXT4_REF`), so the builder is a sibling of its own,
//! `../rust-fs-ext4-image-builder`, checked out at `EXT4_IMAGE_BUILDER_REF`
//! by `chore siblings` and by CI. Every script that starts or reaches the
//! builder must use that checkout: the driver's checkout no longer has it,
//! and a driver bump must not be able to take it away again. These checks
//! read the scripts as text; they need no VM.

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const BUILDER: &str = "../rust-fs-ext4-image-builder/test-disks/";
const DRIVER: &str = "../rust-fs-ext4/test-disks/";

#[test]
fn every_script_reaching_the_builder_uses_the_pinned_builder_checkout() {
    for script in [
        "scripts/run-matrix.sh",
        "scripts/builder-ssh.sh",
        "scripts/test-mount",
        ".github/workflows/matrix.yml",
    ] {
        let text = read(script);
        assert!(
            !text.contains(DRIVER),
            "{script} still reaches the image builder through {DRIVER}, the driver's \
             checkout, which has had no builder since rust-fs-ext4 v0.6.0"
        );
        assert!(
            text.contains(BUILDER) || text.contains("../rust-fs-ext4-image-builder"),
            "{script} does not name the builder checkout ../rust-fs-ext4-image-builder"
        );
    }
}

#[test]
fn chore_siblings_checks_out_the_builder_at_its_own_pin() {
    let chores = read("chores.yml");
    assert!(
        chores.contains(
            "rust-fs-ext4-image-builder '{{.FS_EXT4_URL}}' '{{.EXT4_IMAGE_BUILDER_REF}}'"
        ),
        "chore siblings does not check out rust-fs-ext4-image-builder at EXT4_IMAGE_BUILDER_REF"
    );
}
