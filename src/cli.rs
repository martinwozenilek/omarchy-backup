use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "omarchy-backup",
    version,
    about = "rsync backup of an Omarchy setup to a LUKS2+btrfs USB drive",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub backup: BackupArgs,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a backup (default when no subcommand is given)
    Backup(BackupArgs),
    /// One-time setup: wipe a USB disk and create GPT + LUKS2 + btrfs
    FormatUsb(FormatUsbArgs),
    /// Validate omarchy-backup.toml and print parsed sources
    Check(CheckArgs),
}

#[derive(Debug, Clone, Default, Args)]
pub struct BackupArgs {
    /// Preview what would be synced without writing to the drive
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,

    /// Verbose rsync output
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    /// Unmount, lock, and power off the drive after a successful real backup
    #[arg(long = "eject")]
    pub eject: bool,

    /// Path to config file (overrides automatic discovery)
    #[arg(long = "config", value_name = "FILE", value_parser = non_empty_path)]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct FormatUsbArgs {
    /// Whole USB block device to erase and format (e.g. /dev/sdb)
    #[arg(value_name = "DEVICE", value_parser = non_empty_path)]
    pub device: PathBuf,

    /// Optional config file to read labels and marker from
    #[arg(long = "config", value_name = "FILE", value_parser = non_empty_path)]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct CheckArgs {
    /// Path to config file (overrides automatic discovery)
    #[arg(long = "config", value_name = "FILE", value_parser = non_empty_path)]
    pub config: Option<PathBuf>,
}

fn non_empty_path(s: &str) -> Result<PathBuf, String> {
    if s.is_empty() {
        Err("path must not be empty".to_string())
    } else {
        Ok(PathBuf::from(s))
    }
}
