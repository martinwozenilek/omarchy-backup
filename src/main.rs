use std::process::ExitCode;

use clap::Parser;

use omarchy_backup::backup::run_backup_cli;
use omarchy_backup::cli::{CheckArgs, Cli, Command};
use omarchy_backup::config::{discover_config_path, load_config};
use omarchy_backup::format_usb::run_format_usb;
use omarchy_backup::privilege::resolve_home_best_effort;

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        None => exit_code_from_i32(run_backup_cli(cli.backup)),
        Some(Command::Backup(args)) => exit_code_from_i32(run_backup_cli(args)),
        Some(Command::FormatUsb(args)) => match run_format_usb(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("{err:#}");
                ExitCode::FAILURE
            }
        },
        Some(Command::Check(args)) => match run_check(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("Error: {err:#}");
                ExitCode::FAILURE
            }
        },
    }
}

fn exit_code_from_i32(code: i32) -> ExitCode {
    let byte = u8::try_from(code).unwrap_or(1);
    ExitCode::from(byte)
}

fn run_check(args: CheckArgs) -> anyhow::Result<()> {
    let home = resolve_home_best_effort();
    let cfg_path = discover_config_path(args.config.as_deref(), &home)?;
    let cfg = load_config(&cfg_path, &home)?;
    println!("Config OK: {}", cfg_path.display());
    println!(
        "Settings: fs_label={}, luks_label={}, marker={}, dest_dir={}, keep_deleted_days={}, keep_logs={}, min_free_gib={}",
        cfg.settings.fs_label,
        cfg.settings.luks_label,
        cfg.settings.marker,
        cfg.settings.dest_dir,
        cfg.settings.keep_deleted_days,
        cfg.settings.keep_logs,
        cfg.settings.min_free_gib
    );
    for src in &cfg.sources {
        println!(
            "  {} -> {} (optional={}, vfat={}, one_file_system={}, excludes={})",
            src.path.display(),
            src.dest,
            src.optional,
            src.vfat,
            src.one_file_system,
            src.excludes.len()
        );
    }
    Ok(())
}
