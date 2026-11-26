use assert_cmd::Command;
use predicates::prelude::*;

fn bin() -> Command {
    assert_cmd::cargo::cargo_bin_cmd!("omarchy-backup")
}

#[test]
fn help_exits_zero() {
    bin()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("omarchy-backup"));
}

#[test]
fn unknown_flag_exits_two() {
    bin().arg("--not-a-real-flag").assert().code(2);
}

#[test]
fn missing_config_arg_exits_two() {
    bin().arg("--config").assert().code(2);
}

#[test]
fn empty_config_arg_exits_two() {
    bin().arg("--config=").assert().code(2);
}

#[test]
fn check_validates_repo_config() {
    let repo_cfg = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy-backup.toml");
    bin()
        .args(["check", "--config"])
        .arg(&repo_cfg)
        .assert()
        .success()
        .stdout(predicate::str::contains("Config OK:"));
}
