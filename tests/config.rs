use std::path::Path;

use omarchy_backup::config::{
    Settings, discover_config_path, load_config, parse_and_validate, posix_normpath,
};

const TEST_HOME: &str = "/home/testuser";

fn parse_ok(toml_text: &str) -> omarchy_backup::config::Config {
    parse_and_validate(toml_text, Path::new("test.toml"), Path::new(TEST_HOME))
        .expect("expected config to parse")
}

fn assert_reject(name: &str, toml_text: &str) {
    let res = parse_and_validate(toml_text, Path::new("test.toml"), Path::new(TEST_HOME));
    assert!(res.is_err(), "expected rejection for: {name}");
}

#[test]
fn production_config_parses_cleanly() {
    let repo_cfg = Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy-backup.toml");
    let cfg = load_config(&repo_cfg, Path::new(TEST_HOME)).expect("production config must parse");

    assert_eq!(cfg.settings, Settings::default());

    let home = cfg
        .sources
        .iter()
        .find(|s| s.dest == "home")
        .expect("home source must exist");
    assert_eq!(home.path, Path::new("/home/testuser"));
    assert!(!home.optional);
    assert!(!home.vfat);
    assert!(home.one_file_system);
    assert!(
        home.excludes
            .iter()
            .any(|e| e == "/.local/state/omarchy-backup/")
    );

    let boot = cfg
        .sources
        .iter()
        .find(|s| s.dest == "system/boot")
        .expect("boot source must exist");
    assert!(boot.vfat);
}

#[test]
fn explicit_one_file_system_false() {
    let cfg = parse_ok(
        r#"
[[source]]
path = "/etc"
dest = "system/etc"
one_file_system = false
"#,
    );
    assert_eq!(cfg.sources.len(), 1);
    assert!(!cfg.sources[0].one_file_system);
}

#[test]
fn tilde_expansion_and_normalization() {
    let cfg = parse_ok(
        r#"
[[source]]
path = "~/Documents//notes/."
dest = "docs/./notes/"
"#,
    );
    assert_eq!(
        cfg.sources[0].path,
        Path::new("/home/testuser/Documents/notes")
    );
    assert_eq!(cfg.sources[0].dest, "docs/notes");
}

#[test]
fn custom_settings_override_defaults() {
    let cfg = parse_ok(
        r#"
[settings]
fs_label = "my-backup"
luks_label = "MY-BACKUP"
marker = ".my-marker"
dest_dir = "backups/omarchy"
keep_deleted_days = 14
keep_logs = 20
min_free_gib = 10

[[source]]
path = "/etc"
dest = "system/etc"
"#,
    );
    assert_eq!(cfg.settings.fs_label, "my-backup");
    assert_eq!(cfg.settings.luks_label, "MY-BACKUP");
    assert_eq!(cfg.settings.marker, ".my-marker");
    assert_eq!(cfg.settings.dest_dir, "backups/omarchy");
    assert_eq!(cfg.settings.keep_deleted_days, 14);
    assert_eq!(cfg.settings.keep_logs, 20);
    assert_eq!(cfg.settings.min_free_gib, 10);
}

#[test]
fn rejects_invalid_configs() {
    assert_reject(
        "string boolean optional",
        r#"
[[source]]
path = "/etc"
dest = "system/etc"
optional = "false"
"#,
    );
    assert_reject(
        "string boolean one_file_system",
        r#"
[[source]]
path = "/etc"
dest = "system/etc"
one_file_system = "true"
"#,
    );
    assert_reject(
        "non-table source entry",
        r#"
source = ["not-a-table"]
"#,
    );
    assert_reject(
        "reserved dest LAST_BACKUP",
        r#"
[[source]]
path = "/etc"
dest = "LAST_BACKUP"
"#,
    );
    assert_reject(
        "reserved dest packages/foo",
        r#"
[[source]]
path = "/etc"
dest = "packages/foo"
"#,
    );
    assert_reject(
        "reserved dest _deleted",
        r#"
[[source]]
path = "/etc"
dest = "_deleted"
"#,
    );
    assert_reject(
        "reserved exact dest system",
        r#"
[[source]]
path = "/etc"
dest = "system"
"#,
    );
    assert_reject(
        "overlapping dests parent then child",
        r#"
[[source]]
path = "/etc"
dest = "a"

[[source]]
path = "/usr/local"
dest = "a/b"
"#,
    );
    assert_reject(
        "overlapping dests child then parent",
        r#"
[[source]]
path = "/etc"
dest = "a/b"

[[source]]
path = "/usr/local"
dest = "a"
"#,
    );
    assert_reject(
        "duplicate exact dest",
        r#"
[[source]]
path = "/etc"
dest = "a"

[[source]]
path = "/usr/local"
dest = "a"
"#,
    );
    assert_reject(
        "empty exclude string",
        r#"
[[source]]
path = "/etc"
dest = "system/etc"
excludes = [""]
"#,
    );
    assert_reject(
        "newline in exclude string",
        r#"
[[source]]
path = "/etc"
dest = "system/etc"
excludes = ["foo\nbar"]
"#,
    );
    assert_reject("no sources", "");
    assert_reject(
        "unknown source key",
        r#"
[[source]]
path = "/etc"
dest = "system/etc"
bogus = true
"#,
    );
    assert_reject(
        "relative source path",
        r#"
[[source]]
path = "etc"
dest = "system/etc"
"#,
    );
    assert_reject(
        "absolute dest",
        r#"
[[source]]
path = "/etc"
dest = "/system/etc"
"#,
    );
    assert_reject(
        "parent traversal dest",
        r#"
[[source]]
path = "/etc"
dest = "../escape"
"#,
    );
    assert_reject(
        "dot dest",
        r#"
[[source]]
path = "/etc"
dest = "."
"#,
    );
    assert_reject(
        "tab in path",
        "[[source]]\npath = \"/etc\tfoo\"\ndest = \"system/etc\"\n",
    );
    assert_reject(
        "zero keep_logs",
        r#"
[settings]
keep_logs = 0

[[source]]
path = "/etc"
dest = "system/etc"
"#,
    );
    assert_reject(
        "marker with slash",
        r#"
[settings]
marker = "sub/.marker"

[[source]]
path = "/etc"
dest = "system/etc"
"#,
    );
}

#[test]
fn posix_normpath_cases() {
    assert_eq!(posix_normpath(""), ".");
    assert_eq!(posix_normpath("/"), "/");
    assert_eq!(posix_normpath("/etc/"), "/etc");
    assert_eq!(posix_normpath("/etc/../var/./lib//"), "/var/lib");
    assert_eq!(posix_normpath("/../../etc"), "/etc");
    assert_eq!(posix_normpath("a/b/../c"), "a/c");
    assert_eq!(posix_normpath("a/../../b"), "../b");
    assert_eq!(posix_normpath("./"), ".");
}

#[test]
fn discover_explicit_config_wins() {
    let p = discover_config_path(Some(Path::new("/tmp/custom.toml")), Path::new(TEST_HOME))
        .expect("explicit path returned directly");
    assert_eq!(p, Path::new("/tmp/custom.toml"));
}
