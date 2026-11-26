pub const APP_NAME: &str = "omarchy-backup";
pub const DEFAULT_CONFIG_FILENAME: &str = "omarchy-backup.toml";
pub const DEFAULT_XDG_CONFIG_REL: &str = ".config/omarchy-backup/config.toml";

pub const DEFAULT_FS_LABEL: &str = "omarchy-backup";
pub const DEFAULT_LUKS_LABEL: &str = "OMARCHY-BACKUP";
pub const DEFAULT_MARKER: &str = ".omarchy-backup-target";
pub const DEFAULT_DEST_DIR: &str = "omarchy-backup";
pub const DEFAULT_KEEP_DELETED_DAYS: u64 = 30;
pub const DEFAULT_KEEP_LOGS: usize = 10;
pub const DEFAULT_MIN_FREE_GIB: u64 = 5;

pub const LOCK_FILE_PATH: &str = "/run/omarchy-backup.lock";
pub const LOG_STATE_REL_DIR: &str = ".local/state/omarchy-backup";

pub const FORMAT_MAPPER_NAME: &str = "omarchy-backup";
pub const FORMAT_TEMP_MOUNT: &str = "/mnt/omarchy-backup";

pub const RESERVED_DEST_PREFIXES: &[&str] = &["packages", "_deleted", "LAST_BACKUP"];
pub const RESERVED_DEST_EXACT: &[&str] = &["system"];
