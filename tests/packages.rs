use omarchy_backup::packages::parse_pacman_modified_files;

#[test]
fn parses_modified_files_only() {
    let sample = "\
Name            : filesystem
Backup Files    : /etc/fstab [modified]
                  /etc/group [modified]
                  /etc/hosts [unmodified]
Name            : pacman
Backup Files    : /etc/pacman.conf [unmodified]
";
    let parsed = parse_pacman_modified_files(sample);
    assert_eq!(parsed, "filesystem\t/etc/fstab\nfilesystem\t/etc/group\n");
}

#[test]
fn handles_spaces_in_modified_paths_and_empty_input() {
    assert_eq!(parse_pacman_modified_files(""), "");

    let sample = "\
Name            : my-pkg
Backup Files    : /etc/my app/config.conf [modified]
                  /etc/my app/other file.conf [modified]
";
    let parsed = parse_pacman_modified_files(sample);
    assert_eq!(
        parsed,
        "my-pkg\t/etc/my app/config.conf\nmy-pkg\t/etc/my app/other file.conf\n"
    );
}
