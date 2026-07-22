//! Shared application-level SQLite capacity policy for Mempool Atlas.
//!
//! SQLite's `max_page_count` constrains the main database, but it does not
//! constrain WAL, SHM, or transient filesystem use. This crate combines that
//! page limit with an application envelope and a filesystem reserve. The WAL
//! setting is deliberately a retention and pressure target, not a hard
//! transient ceiling: a transaction may temporarily grow the WAL past it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use rusqlite::Connection;
use thiserror::Error;

const SQLITE_TEMP_STORE_MEMORY: u64 = 2;

/// Application-level bounds for one filesystem-backed SQLite database.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SqliteStorageLimits {
    /// Hard upper bound for the main database, enforced through page count.
    pub max_database_bytes: u64,
    /// Application envelope for the main database plus its WAL and SHM files.
    pub max_total_sqlite_bytes: u64,
    /// Absolute filesystem space that must remain available to the process.
    pub filesystem_reserve_bytes: u64,
    /// Percentage of total filesystem space that must remain available.
    pub filesystem_reserve_percent: u8,
    /// WAL retention and pressure target, not a hard transient WAL ceiling.
    pub retained_wal_high_water_bytes: u64,
    /// Explicit SQLite WAL auto-checkpoint interval in database pages.
    pub wal_autocheckpoint_pages: u32,
}

impl SqliteStorageLimits {
    /// Validate values independently of a particular SQLite page size.
    pub fn validate(&self) -> Result<(), SqliteStorageLimitsError> {
        for (field, value) in [
            ("max_database_bytes", self.max_database_bytes),
            ("max_total_sqlite_bytes", self.max_total_sqlite_bytes),
            ("filesystem_reserve_bytes", self.filesystem_reserve_bytes),
            (
                "retained_wal_high_water_bytes",
                self.retained_wal_high_water_bytes,
            ),
        ] {
            if value == 0 {
                return Err(SqliteStorageLimitsError::ZeroValue { field });
            }
        }

        if self.filesystem_reserve_percent == 0 || self.filesystem_reserve_percent > 100 {
            return Err(
                SqliteStorageLimitsError::FilesystemReservePercentOutOfRange {
                    found: self.filesystem_reserve_percent,
                },
            );
        }
        if self.wal_autocheckpoint_pages == 0 {
            return Err(SqliteStorageLimitsError::ZeroValue {
                field: "wal_autocheckpoint_pages",
            });
        }

        let minimum_envelope = self
            .max_database_bytes
            .checked_add(self.retained_wal_high_water_bytes)
            .ok_or(SqliteStorageLimitsError::MinimumEnvelopeOverflow)?;
        if self.max_database_bytes > i64::MAX as u64 {
            return Err(SqliteStorageLimitsError::SqlitePragmaValueOutOfRange {
                field: "max_database_bytes",
                value: self.max_database_bytes,
            });
        }
        if self.retained_wal_high_water_bytes > i64::MAX as u64 {
            return Err(SqliteStorageLimitsError::SqlitePragmaValueOutOfRange {
                field: "retained_wal_high_water_bytes",
                value: self.retained_wal_high_water_bytes,
            });
        }
        if self.wal_autocheckpoint_pages > i32::MAX as u32 {
            return Err(SqliteStorageLimitsError::WalAutocheckpointOutOfRange {
                found: self.wal_autocheckpoint_pages,
            });
        }
        if self.max_total_sqlite_bytes < minimum_envelope {
            return Err(SqliteStorageLimitsError::TotalEnvelopeTooSmall {
                max_total_sqlite_bytes: self.max_total_sqlite_bytes,
                minimum_bytes: minimum_envelope,
            });
        }

        Ok(())
    }

    fn computed_filesystem_reserve_bytes(
        &self,
        filesystem_total_bytes: u64,
    ) -> Result<u64, SqliteStorageError> {
        self.validate()?;
        let percentage_reserve =
            percentage_ceil(filesystem_total_bytes, self.filesystem_reserve_percent);
        Ok(self.filesystem_reserve_bytes.max(percentage_reserve))
    }
}

/// Invalid static storage-policy configuration.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SqliteStorageLimitsError {
    #[error("{field} must be greater than zero")]
    ZeroValue { field: &'static str },
    #[error("filesystem_reserve_percent must be in 1..=100; found {found}")]
    FilesystemReservePercentOutOfRange { found: u8 },
    #[error("{field} value {value} cannot be represented by a SQLite pragma")]
    SqlitePragmaValueOutOfRange { field: &'static str, value: u64 },
    #[error("wal_autocheckpoint_pages must not exceed {maximum}; found {found}", maximum = i32::MAX)]
    WalAutocheckpointOutOfRange { found: u32 },
    #[error("max_database_bytes plus retained_wal_high_water_bytes overflows u64")]
    MinimumEnvelopeOverflow,
    #[error(
        "max_total_sqlite_bytes is {max_total_sqlite_bytes}, but must be at least {minimum_bytes} bytes to cover the main database and retained WAL target"
    )]
    TotalEnvelopeTooSmall {
        max_total_sqlite_bytes: u64,
        minimum_bytes: u64,
    },
}

/// Physical and SQLite-level capacity state captured for one database.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlitePhysicalSnapshot {
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub shm_bytes: u64,
    pub page_size_bytes: u64,
    pub page_count: u64,
    pub max_page_count: u64,
    pub freelist_count: u64,
    pub filesystem_total_bytes: u64,
    /// Space available to the non-privileged process, as reported by `fs4`.
    pub filesystem_available_bytes: u64,
    pub computed_filesystem_reserve_bytes: u64,
    pub total_sqlite_bytes: u64,
    pub remaining_envelope_bytes: u64,
}

/// Failure to configure, inspect, relieve, or admit SQLite storage.
#[derive(Debug, Error)]
pub enum SqliteStorageError {
    #[error(transparent)]
    InvalidLimits(#[from] SqliteStorageLimitsError),
    #[error("SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("could not inspect SQLite file {path}: {source}")]
    FileMetadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not resolve SQLite database path {path}: {source}")]
    ResolveDatabasePath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "configured SQLite path {provided} does not identify the connection's main database {actual}"
    )]
    DatabasePathMismatch { provided: PathBuf, actual: PathBuf },
    #[error("the SQLite connection's main database is not filesystem-backed")]
    MainDatabaseNotFilesystemBacked,
    #[error("the SQLite main-database path is not valid UTF-8 on this platform")]
    MainDatabasePathEncoding,
    #[error("could not inspect the filesystem containing {path}: {source}")]
    FilesystemStats {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("SQLite pragma {pragma} returned invalid value {value}")]
    InvalidPragmaValue { pragma: &'static str, value: i64 },
    #[error(
        "max_database_bytes {max_database_bytes} is smaller than SQLite page size {page_size_bytes}"
    )]
    MainDatabaseBudgetBelowPageSize {
        max_database_bytes: u64,
        page_size_bytes: u64,
    },
    #[error(
        "existing SQLite page count {page_count} exceeds configured main-database budget of {allowed_page_count} pages"
    )]
    ExistingDatabaseExceedsPageBudget {
        page_count: u64,
        allowed_page_count: u64,
    },
    #[error(
        "SQLite max_page_count was not safely applied: requested at most {requested_page_count}, found {applied_page_count}"
    )]
    MaxPageCountNotApplied {
        requested_page_count: u64,
        applied_page_count: u64,
    },
    #[error("SQLite pragma {pragma} was not applied: expected {expected}, found {found}")]
    PragmaNotApplied {
        pragma: &'static str,
        expected: u64,
        found: u64,
    },
    #[error("capacity arithmetic overflow while computing {context}")]
    CapacityArithmeticOverflow { context: &'static str },
    #[error(
        "main SQLite database uses {database_bytes} bytes, exceeding its {max_database_bytes}-byte limit"
    )]
    MainDatabaseLimitExceeded {
        database_bytes: u64,
        max_database_bytes: u64,
    },
    #[error(
        "SQLite files use {total_sqlite_bytes} bytes, exceeding their {max_total_sqlite_bytes}-byte envelope"
    )]
    TotalSqliteEnvelopeExceeded {
        total_sqlite_bytes: u64,
        max_total_sqlite_bytes: u64,
    },
    #[error(
        "WAL uses {wal_bytes} bytes after optional relief, above the {retained_wal_high_water_bytes}-byte retention and pressure target"
    )]
    WalPressure {
        wal_bytes: u64,
        retained_wal_high_water_bytes: u64,
    },
    #[error(
        "WAL checkpoint was blocked by another SQLite reader or writer (busy={busy}, log_frames={log_frames}, checkpointed_frames={checkpointed_frames})"
    )]
    WalCheckpointBlocked {
        busy: u64,
        log_frames: u64,
        checkpointed_frames: u64,
    },
    #[error("WAL relief requires an autocommit connection outside a transaction")]
    WalReliefRequiresAutocommit,
    #[error(
        "filesystem has {filesystem_available_bytes} bytes available, but admission requires {required_available_bytes} bytes: reserve {computed_filesystem_reserve_bytes} plus remaining SQLite envelope {remaining_envelope_bytes}"
    )]
    FilesystemPressure {
        filesystem_available_bytes: u64,
        required_available_bytes: u64,
        computed_filesystem_reserve_bytes: u64,
        remaining_envelope_bytes: u64,
    },
}

/// Apply and verify the connection-level part of the storage policy.
///
/// The main database budget is rounded down to a whole number of the current
/// SQLite page size. The effective `max_page_count` may be lower than requested
/// if SQLite's compile-time ceiling is lower, but it is never allowed to be
/// higher. Existing databases that already exceed the requested page budget are
/// rejected before configuration.
pub fn configure_sqlite_connection(
    connection: &Connection,
    limits: &SqliteStorageLimits,
) -> Result<(), SqliteStorageError> {
    limits.validate()?;

    let page_size_bytes = pragma_u64(connection, "page_size")?;
    let page_count = pragma_u64(connection, "page_count")?;
    let allowed_page_count = limits.max_database_bytes / page_size_bytes;
    if allowed_page_count == 0 {
        return Err(SqliteStorageError::MainDatabaseBudgetBelowPageSize {
            max_database_bytes: limits.max_database_bytes,
            page_size_bytes,
        });
    }
    if page_count > allowed_page_count {
        return Err(SqliteStorageError::ExistingDatabaseExceedsPageBudget {
            page_count,
            allowed_page_count,
        });
    }

    let requested_page_count = i64::try_from(allowed_page_count).map_err(|_| {
        SqliteStorageError::CapacityArithmeticOverflow {
            context: "max_page_count pragma value",
        }
    })?;
    let applied_page_count: i64 = connection.pragma_update_and_check(
        Some("main"),
        "max_page_count",
        requested_page_count,
        |row| row.get(0),
    )?;
    let applied_page_count = pragma_value_to_u64("max_page_count", applied_page_count)?;
    if applied_page_count > allowed_page_count || applied_page_count < page_count {
        return Err(SqliteStorageError::MaxPageCountNotApplied {
            requested_page_count: allowed_page_count,
            applied_page_count,
        });
    }

    let journal_size_limit = i64::try_from(limits.retained_wal_high_water_bytes)
        .expect("validated retained WAL target fits i64");
    connection.pragma_update(None, "journal_size_limit", journal_size_limit)?;
    verify_pragma(
        connection,
        "journal_size_limit",
        limits.retained_wal_high_water_bytes,
    )?;

    connection.pragma_update(
        None,
        "wal_autocheckpoint",
        i64::from(limits.wal_autocheckpoint_pages),
    )?;
    verify_pragma(
        connection,
        "wal_autocheckpoint",
        u64::from(limits.wal_autocheckpoint_pages),
    )?;

    connection.pragma_update(None, "temp_store", "MEMORY")?;
    verify_pragma(connection, "temp_store", SQLITE_TEMP_STORE_MEMORY)?;

    Ok(())
}

/// Capture SQLite file sizes, page accounting, and filesystem capacity.
///
/// `database_path` must identify the filesystem-backed `main` database opened
/// by `connection`.
pub fn sqlite_physical_snapshot(
    connection: &Connection,
    database_path: impl AsRef<Path>,
    limits: &SqliteStorageLimits,
) -> Result<SqlitePhysicalSnapshot, SqliteStorageError> {
    limits.validate()?;
    let database_path = resolve_main_database_path(connection, database_path.as_ref())?;
    let wal_path = sqlite_sidecar_path(&database_path, "-wal");
    let shm_path = sqlite_sidecar_path(&database_path, "-shm");

    let database_bytes = file_size_or_zero(&database_path)?;
    let wal_bytes = file_size_or_zero(&wal_path)?;
    let shm_bytes = file_size_or_zero(&shm_path)?;
    let total_sqlite_bytes = database_bytes
        .checked_add(wal_bytes)
        .and_then(|bytes| bytes.checked_add(shm_bytes))
        .ok_or(SqliteStorageError::CapacityArithmeticOverflow {
            context: "database, WAL, and SHM total",
        })?;

    let filesystem_path = filesystem_probe_path(&database_path);
    let filesystem_stats =
        fs4::statvfs(&filesystem_path).map_err(|source| SqliteStorageError::FilesystemStats {
            path: filesystem_path.clone(),
            source,
        })?;
    let filesystem_total_bytes = filesystem_stats.total_space();
    let filesystem_available_bytes = filesystem_stats.available_space();
    let computed_filesystem_reserve_bytes =
        limits.computed_filesystem_reserve_bytes(filesystem_total_bytes)?;

    Ok(SqlitePhysicalSnapshot {
        database_bytes,
        wal_bytes,
        shm_bytes,
        page_size_bytes: pragma_u64(connection, "page_size")?,
        page_count: pragma_u64(connection, "page_count")?,
        max_page_count: pragma_u64(connection, "max_page_count")?,
        freelist_count: pragma_u64(connection, "freelist_count")?,
        filesystem_total_bytes,
        filesystem_available_bytes,
        computed_filesystem_reserve_bytes,
        total_sqlite_bytes,
        remaining_envelope_bytes: limits
            .max_total_sqlite_bytes
            .saturating_sub(total_sqlite_bytes),
    })
}

/// Attempt to return an oversized WAL to its retention and pressure target.
///
/// This function only issues `PRAGMA wal_checkpoint(TRUNCATE)` when the WAL is
/// above `retained_wal_high_water_bytes`. It must be called outside a
/// transaction. If the checkpoint is blocked or insufficient and the WAL
/// remains above the target, [`SqliteStorageError::WalPressure`] is returned.
/// A checkpoint blocked by a concurrent reader or writer is reported
/// separately as [`SqliteStorageError::WalCheckpointBlocked`]. The target is
/// not a hard transient ceiling.
pub fn relieve_wal_pressure(
    connection: &Connection,
    database_path: impl AsRef<Path>,
    limits: &SqliteStorageLimits,
) -> Result<SqlitePhysicalSnapshot, SqliteStorageError> {
    let database_path = database_path.as_ref();
    let before = sqlite_physical_snapshot(connection, database_path, limits)?;
    if before.wal_bytes <= limits.retained_wal_high_water_bytes {
        return Ok(before);
    }
    if !connection.is_autocommit() {
        return Err(SqliteStorageError::WalReliefRequiresAutocommit);
    }

    let (busy, log_frames, checkpointed_frames): (i64, i64, i64) =
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let busy = pragma_value_to_u64("wal_checkpoint.busy", busy)?;
    let log_frames = pragma_value_to_u64("wal_checkpoint.log_frames", log_frames)?;
    let checkpointed_frames =
        pragma_value_to_u64("wal_checkpoint.checkpointed_frames", checkpointed_frames)?;
    if busy != 0 {
        return Err(SqliteStorageError::WalCheckpointBlocked {
            busy,
            log_frames,
            checkpointed_frames,
        });
    }
    let after = sqlite_physical_snapshot(connection, database_path, limits)?;
    ensure_wal_within_target(&after, limits)?;
    Ok(after)
}

/// Check whether a write may begin without changing SQLite state.
///
/// This function does not checkpoint or update pragmas and is safe to call
/// inside an already-open transaction. It rejects writes when the WAL is above
/// its pressure target, the main database or total SQLite envelope is exceeded,
/// or non-privileged available space cannot preserve both the filesystem
/// reserve and all remaining growth permitted by the SQLite envelope.
pub fn check_write_admission(
    connection: &Connection,
    database_path: impl AsRef<Path>,
    limits: &SqliteStorageLimits,
) -> Result<SqlitePhysicalSnapshot, SqliteStorageError> {
    let snapshot = sqlite_physical_snapshot(connection, database_path, limits)?;
    evaluate_write_admission(&snapshot, limits)?;
    Ok(snapshot)
}

/// Relieve WAL pressure and then check write admission outside a transaction.
///
/// Callers that are already in a transaction must use
/// [`check_write_admission`] directly. This convenience function provides the
/// optional pre-transaction checkpoint path without hiding a mutation inside
/// the observational admission check.
pub fn prepare_for_sqlite_write(
    connection: &Connection,
    database_path: impl AsRef<Path>,
    limits: &SqliteStorageLimits,
) -> Result<SqlitePhysicalSnapshot, SqliteStorageError> {
    let database_path = database_path.as_ref();
    relieve_wal_pressure(connection, database_path, limits)?;
    check_write_admission(connection, database_path, limits)
}

fn evaluate_write_admission(
    snapshot: &SqlitePhysicalSnapshot,
    limits: &SqliteStorageLimits,
) -> Result<(), SqliteStorageError> {
    limits.validate()?;
    ensure_wal_within_target(snapshot, limits)?;

    if snapshot.database_bytes > limits.max_database_bytes {
        return Err(SqliteStorageError::MainDatabaseLimitExceeded {
            database_bytes: snapshot.database_bytes,
            max_database_bytes: limits.max_database_bytes,
        });
    }
    if snapshot.total_sqlite_bytes > limits.max_total_sqlite_bytes {
        return Err(SqliteStorageError::TotalSqliteEnvelopeExceeded {
            total_sqlite_bytes: snapshot.total_sqlite_bytes,
            max_total_sqlite_bytes: limits.max_total_sqlite_bytes,
        });
    }

    let required_available_bytes = snapshot
        .computed_filesystem_reserve_bytes
        .checked_add(snapshot.remaining_envelope_bytes)
        .ok_or(SqliteStorageError::CapacityArithmeticOverflow {
            context: "filesystem reserve plus remaining SQLite envelope",
        })?;
    if snapshot.filesystem_available_bytes < required_available_bytes {
        return Err(SqliteStorageError::FilesystemPressure {
            filesystem_available_bytes: snapshot.filesystem_available_bytes,
            required_available_bytes,
            computed_filesystem_reserve_bytes: snapshot.computed_filesystem_reserve_bytes,
            remaining_envelope_bytes: snapshot.remaining_envelope_bytes,
        });
    }

    Ok(())
}

fn ensure_wal_within_target(
    snapshot: &SqlitePhysicalSnapshot,
    limits: &SqliteStorageLimits,
) -> Result<(), SqliteStorageError> {
    if snapshot.wal_bytes > limits.retained_wal_high_water_bytes {
        return Err(SqliteStorageError::WalPressure {
            wal_bytes: snapshot.wal_bytes,
            retained_wal_high_water_bytes: limits.retained_wal_high_water_bytes,
        });
    }
    Ok(())
}

fn percentage_ceil(total: u64, percent: u8) -> u64 {
    let percent = u64::from(percent);
    let whole = (total / 100) * percent;
    let remainder_product = (total % 100) * percent;
    whole + remainder_product.div_ceil(100)
}

fn pragma_u64(connection: &Connection, pragma: &'static str) -> Result<u64, SqliteStorageError> {
    let value: i64 = connection.pragma_query_value(None, pragma, |row| row.get(0))?;
    pragma_value_to_u64(pragma, value)
}

fn pragma_value_to_u64(pragma: &'static str, value: i64) -> Result<u64, SqliteStorageError> {
    u64::try_from(value).map_err(|_| SqliteStorageError::InvalidPragmaValue { pragma, value })
}

fn verify_pragma(
    connection: &Connection,
    pragma: &'static str,
    expected: u64,
) -> Result<(), SqliteStorageError> {
    let found = pragma_u64(connection, pragma)?;
    if found != expected {
        return Err(SqliteStorageError::PragmaNotApplied {
            pragma,
            expected,
            found,
        });
    }
    Ok(())
}

fn sqlite_sidecar_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut path = database_path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn resolve_main_database_path(
    connection: &Connection,
    provided_path: &Path,
) -> Result<PathBuf, SqliteStorageError> {
    let mut statement = connection.prepare("PRAGMA database_list")?;
    let mut rows = statement.query([])?;
    let mut sqlite_path = None;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "main" {
            let value = row.get_ref(2)?;
            let rusqlite::types::ValueRef::Text(bytes) = value else {
                return Err(SqliteStorageError::MainDatabaseNotFilesystemBacked);
            };
            sqlite_path = Some(sqlite_path_from_bytes(bytes)?);
            break;
        }
    }
    let sqlite_path = sqlite_path
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(SqliteStorageError::MainDatabaseNotFilesystemBacked)?;
    let actual = fs::canonicalize(&sqlite_path).map_err(|source| {
        SqliteStorageError::ResolveDatabasePath {
            path: sqlite_path,
            source,
        }
    })?;
    let provided = fs::canonicalize(provided_path).map_err(|source| {
        SqliteStorageError::ResolveDatabasePath {
            path: provided_path.to_path_buf(),
            source,
        }
    })?;
    if provided != actual {
        return Err(SqliteStorageError::DatabasePathMismatch { provided, actual });
    }
    Ok(actual)
}

#[cfg(unix)]
fn sqlite_path_from_bytes(bytes: &[u8]) -> Result<PathBuf, SqliteStorageError> {
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn sqlite_path_from_bytes(bytes: &[u8]) -> Result<PathBuf, SqliteStorageError> {
    let path = String::from_utf8(bytes.to_vec())
        .map_err(|_| SqliteStorageError::MainDatabasePathEncoding)?;
    Ok(PathBuf::from(path))
}

fn file_size_or_zero(path: &Path) -> Result<u64, SqliteStorageError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(source) => Err(SqliteStorageError::FileMetadata {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn filesystem_probe_path(database_path: &Path) -> PathBuf {
    if database_path.exists() {
        return database_path.to_path_buf();
    }
    database_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::{Connection, params};
    use tempfile::TempDir;

    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn valid_limits() -> SqliteStorageLimits {
        SqliteStorageLimits {
            max_database_bytes: 8 * MIB,
            max_total_sqlite_bytes: 16 * MIB,
            filesystem_reserve_bytes: 1,
            filesystem_reserve_percent: 1,
            retained_wal_high_water_bytes: 4 * MIB,
            wal_autocheckpoint_pages: 128,
        }
    }

    fn test_database() -> (TempDir, PathBuf, Connection) {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("atlas.db");
        let connection = Connection::open(&path).expect("open database");
        (directory, path, connection)
    }

    fn synthetic_snapshot(limits: &SqliteStorageLimits) -> SqlitePhysicalSnapshot {
        SqlitePhysicalSnapshot {
            database_bytes: 10,
            wal_bytes: 5,
            shm_bytes: 3,
            page_size_bytes: 4096,
            page_count: 1,
            max_page_count: 10,
            freelist_count: 0,
            filesystem_total_bytes: 1_000,
            filesystem_available_bytes: 1_000,
            computed_filesystem_reserve_bytes: 10,
            total_sqlite_bytes: 18,
            remaining_envelope_bytes: limits.max_total_sqlite_bytes - 18,
        }
    }

    #[test]
    fn validates_nonzero_ranges_and_minimum_envelope() {
        let base = valid_limits();
        for field in [
            "max_database_bytes",
            "max_total_sqlite_bytes",
            "filesystem_reserve_bytes",
            "retained_wal_high_water_bytes",
            "wal_autocheckpoint_pages",
        ] {
            let mut limits = base;
            match field {
                "max_database_bytes" => limits.max_database_bytes = 0,
                "max_total_sqlite_bytes" => limits.max_total_sqlite_bytes = 0,
                "filesystem_reserve_bytes" => limits.filesystem_reserve_bytes = 0,
                "retained_wal_high_water_bytes" => limits.retained_wal_high_water_bytes = 0,
                "wal_autocheckpoint_pages" => limits.wal_autocheckpoint_pages = 0,
                _ => unreachable!(),
            }
            assert_eq!(
                limits.validate(),
                Err(SqliteStorageLimitsError::ZeroValue { field })
            );
        }

        for found in [0, 101] {
            let mut limits = base;
            limits.filesystem_reserve_percent = found;
            assert_eq!(
                limits.validate(),
                Err(SqliteStorageLimitsError::FilesystemReservePercentOutOfRange { found })
            );
        }

        let limits = SqliteStorageLimits {
            max_total_sqlite_bytes: 11,
            max_database_bytes: 8,
            retained_wal_high_water_bytes: 4,
            ..base
        };
        assert_eq!(
            limits.validate(),
            Err(SqliteStorageLimitsError::TotalEnvelopeTooSmall {
                max_total_sqlite_bytes: 11,
                minimum_bytes: 12,
            })
        );

        let limits = SqliteStorageLimits {
            max_database_bytes: u64::MAX,
            retained_wal_high_water_bytes: 1,
            max_total_sqlite_bytes: u64::MAX,
            ..base
        };
        assert!(matches!(
            limits.validate(),
            Err(SqliteStorageLimitsError::MinimumEnvelopeOverflow)
        ));
    }

    #[test]
    fn reserve_arithmetic_rounds_up_without_overflow() {
        assert_eq!(percentage_ceil(0, 1), 0);
        assert_eq!(percentage_ceil(100, 1), 1);
        assert_eq!(percentage_ceil(101, 1), 2);
        assert_eq!(percentage_ceil(u64::MAX, 100), u64::MAX);

        let limits = valid_limits();
        let mut snapshot = synthetic_snapshot(&limits);
        snapshot.computed_filesystem_reserve_bytes = u64::MAX;
        snapshot.remaining_envelope_bytes = 1;
        assert!(matches!(
            evaluate_write_admission(&snapshot, &limits),
            Err(SqliteStorageError::CapacityArithmeticOverflow {
                context: "filesystem reserve plus remaining SQLite envelope"
            })
        ));
    }

    #[test]
    fn configures_and_verifies_sqlite_pragmas() {
        let (_directory, _path, connection) = test_database();
        connection
            .execute_batch("CREATE TABLE records (value BLOB NOT NULL);")
            .expect("create table");
        let page_size = pragma_u64(&connection, "page_size").expect("page size");
        let page_count = pragma_u64(&connection, "page_count").expect("page count");
        let limits = SqliteStorageLimits {
            max_database_bytes: page_size * (page_count + 4),
            max_total_sqlite_bytes: page_size * (page_count + 4) + MIB,
            retained_wal_high_water_bytes: MIB,
            wal_autocheckpoint_pages: 37,
            ..valid_limits()
        };

        configure_sqlite_connection(&connection, &limits).expect("configure storage");

        assert_eq!(
            pragma_u64(&connection, "max_page_count").expect("max pages"),
            page_count + 4
        );
        assert_eq!(
            pragma_u64(&connection, "journal_size_limit").expect("journal limit"),
            MIB
        );
        assert_eq!(
            pragma_u64(&connection, "wal_autocheckpoint").expect("autocheckpoint"),
            37
        );
        assert_eq!(
            pragma_u64(&connection, "temp_store").expect("temp store"),
            SQLITE_TEMP_STORE_MEMORY
        );
    }

    #[test]
    fn rejects_existing_database_over_main_page_budget() {
        let (_directory, _path, connection) = test_database();
        connection
            .execute_batch("CREATE TABLE records (value BLOB NOT NULL);")
            .expect("create table");
        let page_size = pragma_u64(&connection, "page_size").expect("page size");
        let page_count = pragma_u64(&connection, "page_count").expect("page count");
        assert!(page_count > 1);
        let limits = SqliteStorageLimits {
            max_database_bytes: page_size * (page_count - 1),
            max_total_sqlite_bytes: page_size * (page_count - 1) + MIB,
            retained_wal_high_water_bytes: MIB,
            ..valid_limits()
        };

        assert!(matches!(
            configure_sqlite_connection(&connection, &limits),
            Err(SqliteStorageError::ExistingDatabaseExceedsPageBudget {
                page_count: found,
                allowed_page_count,
            }) if found == page_count && allowed_page_count == page_count - 1
        ));
    }

    #[test]
    fn snapshot_accounts_for_database_wal_and_shm_files() {
        let (_directory, path, connection) = test_database();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        connection
            .execute_batch(
                "CREATE TABLE records (value BLOB NOT NULL);\n\
                 INSERT INTO records VALUES (zeroblob(8192));",
            )
            .expect("populate WAL");

        let limits = valid_limits();
        let snapshot = sqlite_physical_snapshot(&connection, &path, &limits).expect("snapshot");
        let expected_database = fs::metadata(&path).expect("database metadata").len();
        let expected_wal = fs::metadata(sqlite_sidecar_path(&path, "-wal"))
            .expect("WAL metadata")
            .len();
        let expected_shm = fs::metadata(sqlite_sidecar_path(&path, "-shm"))
            .expect("SHM metadata")
            .len();

        assert_eq!(snapshot.database_bytes, expected_database);
        assert_eq!(snapshot.wal_bytes, expected_wal);
        assert_eq!(snapshot.shm_bytes, expected_shm);
        assert_eq!(
            snapshot.total_sqlite_bytes,
            expected_database + expected_wal + expected_shm
        );
        assert_eq!(
            snapshot.remaining_envelope_bytes,
            limits.max_total_sqlite_bytes - snapshot.total_sqlite_bytes
        );
        assert!(snapshot.filesystem_total_bytes > 0);
        assert!(snapshot.filesystem_available_bytes <= snapshot.filesystem_total_bytes);
        assert_eq!(
            snapshot.computed_filesystem_reserve_bytes,
            percentage_ceil(snapshot.filesystem_total_bytes, 1)
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_follows_sqlite_main_path_through_a_database_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temp directory");
        let real_directory = directory.path().join("real");
        fs::create_dir(&real_directory).expect("real directory");
        let real_path = real_directory.join("atlas.db");
        Connection::open(&real_path)
            .expect("create real database")
            .close()
            .expect("close real database");
        let alias_path = directory.path().join("atlas-link.db");
        symlink(&real_path, &alias_path).expect("database symlink");

        let connection = Connection::open(&alias_path).expect("open database through symlink");
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        connection
            .execute_batch(
                "CREATE TABLE records (value BLOB NOT NULL);\n\
                 INSERT INTO records VALUES (zeroblob(8192));",
            )
            .expect("populate WAL");

        let snapshot = sqlite_physical_snapshot(&connection, &alias_path, &valid_limits())
            .expect("snapshot through symlink");
        let real_wal = sqlite_sidecar_path(&real_path, "-wal");
        let real_shm = sqlite_sidecar_path(&real_path, "-shm");
        assert!(!sqlite_sidecar_path(&alias_path, "-wal").exists());
        assert_eq!(
            snapshot.database_bytes,
            fs::metadata(&real_path).expect("database metadata").len()
        );
        assert_eq!(
            snapshot.wal_bytes,
            fs::metadata(real_wal).expect("real WAL metadata").len()
        );
        assert_eq!(
            snapshot.shm_bytes,
            fs::metadata(real_shm).expect("real SHM metadata").len()
        );
        assert!(snapshot.wal_bytes > 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn snapshot_accepts_a_non_utf8_unix_database_path() {
        use std::os::unix::ffi::OsStringExt;

        let directory = tempfile::tempdir().expect("temp directory");
        let filename = OsString::from_vec(b"atlas-\xff.db".to_vec());
        let path = directory.path().join(filename);
        let connection = Connection::open(&path).expect("open non-UTF-8 database path");
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        connection
            .execute_batch(
                "CREATE TABLE records (value BLOB NOT NULL);\n\
                 INSERT INTO records VALUES (zeroblob(8192));",
            )
            .expect("populate WAL");

        let snapshot = sqlite_physical_snapshot(&connection, &path, &valid_limits())
            .expect("snapshot non-UTF-8 database path");
        assert!(snapshot.database_bytes > 0);
        assert!(snapshot.wal_bytes > 0);
        assert!(snapshot.shm_bytes > 0);
    }

    #[test]
    fn admission_rejects_each_capacity_pressure_deterministically() {
        let limits = SqliteStorageLimits {
            max_database_bytes: 100,
            max_total_sqlite_bytes: 200,
            retained_wal_high_water_bytes: 50,
            ..valid_limits()
        };

        let mut snapshot = synthetic_snapshot(&limits);
        snapshot.wal_bytes = 51;
        assert!(matches!(
            evaluate_write_admission(&snapshot, &limits),
            Err(SqliteStorageError::WalPressure {
                wal_bytes: 51,
                retained_wal_high_water_bytes: 50
            })
        ));

        let mut snapshot = synthetic_snapshot(&limits);
        snapshot.database_bytes = 101;
        assert!(matches!(
            evaluate_write_admission(&snapshot, &limits),
            Err(SqliteStorageError::MainDatabaseLimitExceeded {
                database_bytes: 101,
                max_database_bytes: 100
            })
        ));

        let mut snapshot = synthetic_snapshot(&limits);
        snapshot.total_sqlite_bytes = 201;
        snapshot.remaining_envelope_bytes = 0;
        assert!(matches!(
            evaluate_write_admission(&snapshot, &limits),
            Err(SqliteStorageError::TotalSqliteEnvelopeExceeded {
                total_sqlite_bytes: 201,
                max_total_sqlite_bytes: 200
            })
        ));

        let mut snapshot = synthetic_snapshot(&limits);
        snapshot.filesystem_available_bytes = 191;
        snapshot.computed_filesystem_reserve_bytes = 10;
        snapshot.remaining_envelope_bytes = 182;
        assert!(matches!(
            evaluate_write_admission(&snapshot, &limits),
            Err(SqliteStorageError::FilesystemPressure {
                filesystem_available_bytes: 191,
                required_available_bytes: 192,
                computed_filesystem_reserve_bytes: 10,
                remaining_envelope_bytes: 182,
            })
        ));
    }

    #[test]
    fn wal_relief_reports_pressure_while_a_reader_pins_the_wal() {
        let (_directory, path, writer) = test_database();
        writer
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        writer
            .execute_batch(
                "CREATE TABLE records (id INTEGER PRIMARY KEY, value BLOB NOT NULL);\n\
                 INSERT INTO records (value) VALUES (zeroblob(1024));\n\
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .expect("create baseline");

        let mut reader = Connection::open(&path).expect("open reader");
        let reader_transaction = reader.transaction().expect("reader transaction");
        let _: i64 = reader_transaction
            .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
            .expect("pin read snapshot");

        writer
            .execute(
                "INSERT INTO records (value) VALUES (?1)",
                params![vec![7_u8; 32 * 1024]],
            )
            .expect("grow WAL");
        let mut limits = valid_limits();
        limits.retained_wal_high_water_bytes = 1;
        limits.max_total_sqlite_bytes = limits.max_database_bytes + 1;

        assert!(matches!(
            relieve_wal_pressure(&writer, &path, &limits),
            Err(SqliteStorageError::WalCheckpointBlocked {
                busy,
                log_frames,
                checkpointed_frames,
            }) if busy > 0 && log_frames > checkpointed_frames
        ));

        drop(reader_transaction);
        let snapshot =
            relieve_wal_pressure(&writer, &path, &limits).expect("truncate WAL after reader exits");
        assert!(snapshot.wal_bytes <= 1);
    }

    #[test]
    fn observational_admission_is_safe_inside_a_transaction() {
        let (_directory, path, mut connection) = test_database();
        connection
            .execute_batch("CREATE TABLE records (value INTEGER NOT NULL);")
            .expect("create table");
        let limits = valid_limits();
        let transaction = connection.transaction().expect("transaction");

        let snapshot = check_write_admission(&transaction, &path, &limits)
            .expect("admission inside transaction");

        assert!(!transaction.is_autocommit());
        assert!(snapshot.total_sqlite_bytes <= limits.max_total_sqlite_bytes);
    }
}
