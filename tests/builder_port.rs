//! The ext4 builder VM's SSH port must follow `EXT4_BUILDER_PORT`.
//!
//! A port fixed at 2222 collides with any other VM already forwarding
//! `localhost:2222`, and ssh then talks to the wrong machine. These checks
//! need no VM: `builder-ssh.sh` is run against a stand-in `ssh` that only
//! records its arguments, and `run-matrix.sh` is read as text.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Runs `scripts/builder-ssh.sh` with a fake ssh and returns the argv it
/// would have passed to the real one, one argument per element.
fn builder_ssh_args(port: Option<&str>) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!(
        "builder-port-{}-{}",
        std::process::id(),
        port.unwrap_or("unset")
    ));
    fs::create_dir_all(&dir).unwrap();
    let fake_ssh = dir.join("ssh");
    fs::write(&fake_ssh, "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\"\n").unwrap();
    fs::set_permissions(&fake_ssh, fs::Permissions::from_mode(0o755)).unwrap();

    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("scripts/builder-ssh.sh"))
        .arg("true")
        .env("EXT4_REAL_SSH", &fake_ssh)
        .env_remove("EXT4_BUILDER_PORT");
    if let Some(p) = port {
        cmd.env("EXT4_BUILDER_PORT", p);
    }
    let out = cmd.output().unwrap();
    fs::remove_dir_all(&dir).unwrap();
    assert!(
        out.status.success(),
        "builder-ssh.sh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn port_of(args: &[String]) -> &str {
    let i = args
        .iter()
        .position(|a| a == "-p")
        .unwrap_or_else(|| panic!("no -p in {args:?}"));
    &args[i + 1]
}

#[test]
fn builder_ssh_connects_on_the_configured_port() {
    let args = builder_ssh_args(Some("2299"));
    assert_eq!(port_of(&args), "2299", "argv: {args:?}");
}

#[test]
fn builder_ssh_defaults_to_2222_when_unset() {
    let args = builder_ssh_args(None);
    assert_eq!(port_of(&args), "2222", "argv: {args:?}");
}

#[test]
fn builder_ssh_offers_only_the_builder_key() {
    let args = builder_ssh_args(Some("2299"));
    assert!(
        args.windows(2)
            .any(|w| w[0] == "-o" && w[1] == "IdentitiesOnly=yes"),
        "argv: {args:?}"
    );
}

fn run_matrix() -> String {
    fs::read_to_string(repo_root().join("scripts/run-matrix.sh")).unwrap()
}

/// The body of a shell function `name() { ... }`, up to the first `}` at
/// column 0.
fn shell_function<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src
        .find(&format!("\n{name}() {{"))
        .unwrap_or_else(|| panic!("{name}() not found"));
    let body = &src[start + 1..];
    let end = body.find("\n}").expect("unterminated function");
    &body[..end]
}

#[test]
fn run_matrix_moves_the_builder_off_2222() {
    let src = run_matrix();
    let export = src
        .lines()
        .find(|l| l.trim_start().starts_with("export EXT4_BUILDER_PORT="))
        .expect("run-matrix.sh does not export EXT4_BUILDER_PORT");
    assert!(
        !export.contains("2222"),
        "default builder port is still 2222: {export}"
    );
}

#[test]
fn run_matrix_shutdown_uses_the_configured_port() {
    let src = run_matrix();
    let body = shell_function(&src, "stop_builder_vm");
    assert!(!body.contains("-p 2222"), "port hard-coded:\n{body}");
    assert!(
        body.contains("-p \"$EXT4_BUILDER_PORT\""),
        "shutdown ssh does not use EXT4_BUILDER_PORT:\n{body}"
    );
    assert!(
        body.contains("IdentitiesOnly=yes"),
        "shutdown ssh offers every agent key:\n{body}"
    );
}

#[test]
fn run_matrix_image_dir_is_ignored() {
    let ignore = fs::read_to_string(repo_root().join(".gitignore")).unwrap();
    assert!(
        ignore.lines().any(|l| l.trim() == "diskimages/"),
        "diskimages/ is not in .gitignore"
    );
}
