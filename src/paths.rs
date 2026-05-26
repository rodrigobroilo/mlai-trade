// Runtime path layout and permission hardening.
//
// Function map:
// - *_dir(): resolve the configured ~/mlai-trade runtime folders.
// - path_in_runtime_dir(): keeps relative/blank config paths inside safe dirs.
// - ensure_*()/harden_*(): create files/dirs with private permissions.
// - named_path_in(): preserves legacy data filenames while moving to new layout.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const RUNTIME_METADATA_FILE_MODE: u32 = 0o644;

// Handles home dir logic.
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(dirs::home_dir)
        .expect("unable to determine home directory; set MLAI_TRADE_HOME explicitly")
}

// Handles expand tilde logic.
fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

// Returns the runtime path for clean relative path.
fn clean_relative_path(path: &Path) -> Option<PathBuf> {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => clean.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if clean.as_os_str().is_empty() {
        None
    } else {
        Some(clean)
    }
}

// Returns the runtime path for in runtime dir.
pub fn path_in_runtime_dir(
    base: PathBuf,
    configured: Option<String>,
    default_name: &str,
) -> PathBuf {
    let Some(value) = configured
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return base.join(default_name);
    };

    let expanded = expand_tilde(&value);
    if expanded.is_absolute() {
        if expanded.starts_with(&base) {
            return expanded;
        }
        return expanded
            .file_name()
            .map(|name| base.join(name))
            .unwrap_or_else(|| base.join(default_name));
    }

    clean_relative_path(&expanded)
        .map(|relative| {
            let starts_with_base_name = base
                .file_name()
                .is_some_and(|name| relative.starts_with(Path::new(name)));
            if starts_with_base_name {
                root_dir().join(relative)
            } else {
                base.join(relative)
            }
        })
        .unwrap_or_else(|| base.join(default_name))
}

#[cfg(unix)]
// Sets mode in local state.
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
// Sets mode in local state.
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

// Handles harden dir if exists logic.
pub fn harden_dir_if_exists(path: &Path) -> io::Result<()> {
    if path.exists() {
        set_mode(path, PRIVATE_DIR_MODE)?;
    }
    Ok(())
}

// Handles harden file if exists logic.
pub fn harden_file_if_exists(path: &Path) -> io::Result<()> {
    if path.exists() {
        set_mode(path, PRIVATE_FILE_MODE)?;
    }
    Ok(())
}

// Returns the runtime path for with appended suffix.
fn path_with_appended_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

// Handles harden sqlite files logic.
pub fn harden_sqlite_files(path: &Path) -> io::Result<()> {
    harden_file_if_exists(path)?;
    harden_file_if_exists(&path_with_appended_suffix(path, "-wal"))?;
    harden_file_if_exists(&path_with_appended_suffix(path, "-shm"))?;
    Ok(())
}

// Handles harden runtime metadata file if exists logic.
pub fn harden_runtime_metadata_file_if_exists(path: &Path) -> io::Result<()> {
    if path.exists() {
        set_mode(path, RUNTIME_METADATA_FILE_MODE)?;
    }
    Ok(())
}

// Ensures private dir exists or meets required invariants.
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    harden_dir_if_exists(path)
}

// Handles create private file logic.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    harden_file_if_exists(path)?;
    Ok(file)
}

// Opens private append with the configured runtime settings.
pub fn open_private_append(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    harden_file_if_exists(path)?;
    Ok(file)
}

// Writes private file to disk or storage.
pub fn write_private_file(path: &Path, content: impl AsRef<[u8]>) -> io::Result<()> {
    let mut file = create_private_file(path)?;
    file.write_all(content.as_ref())?;
    file.flush()
}

// Writes runtime metadata file to disk or storage.
pub fn write_runtime_metadata_file(path: &Path, content: impl AsRef<[u8]>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    harden_runtime_metadata_file_if_exists(path)?;
    file.write_all(content.as_ref())?;
    file.flush()
}

// Handles harden tree logic.
fn harden_tree(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        harden_dir_if_exists(path)?;
        for entry in fs::read_dir(path)? {
            harden_tree(&entry?.path())?;
        }
    } else if metadata.is_file() {
        harden_file_if_exists(path)?;
    }
    Ok(())
}

// Handles harden sensitive runtime permissions logic.
pub fn harden_sensitive_runtime_permissions() -> anyhow::Result<()> {
    harden_dir_if_exists(&root_dir())?;
    for dir in [
        config_dir(),
        data_dir(),
        db_dir(),
        logs_dir(),
        archived_logs_dir(),
        api_dir(),
    ] {
        harden_tree(&dir)?;
    }
    harden_dir_if_exists(&tmp_dir())?;
    for name in ["mlai-trade-daemon.pid", "mlai-trade-api.pid"] {
        harden_runtime_metadata_file_if_exists(&tmp_dir().join(name))?;
    }
    harden_file_if_exists(&tmp_dir().join("mlai-trade-daily-refresh.stamp"))?;
    Ok(())
}

// Handles root dir logic.
pub fn root_dir() -> PathBuf {
    std::env::var("MLAI_TRADE_HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| expand_tilde(value.trim()))
        .unwrap_or_else(|| home_dir().join("mlai-trade"))
}

// Returns Alpaca data-feed dir information.
pub fn data_dir() -> PathBuf {
    root_dir().join("data")
}

// Handles db dir logic.
pub fn db_dir() -> PathBuf {
    root_dir().join("db")
}

// Builds or returns dir configuration state.
pub fn config_dir() -> PathBuf {
    root_dir().join("config")
}

// Handles docs dir logic.
pub fn docs_dir() -> PathBuf {
    root_dir().join("docs")
}

// Handles logs dir logic.
pub fn logs_dir() -> PathBuf {
    root_dir().join("logs")
}

// Handles archived logs dir logic.
pub fn archived_logs_dir() -> PathBuf {
    logs_dir().join("archived")
}

// Runs the api dir API helper.
pub fn api_dir() -> PathBuf {
    root_dir().join("api")
}

// Handles tmp dir logic.
pub fn tmp_dir() -> PathBuf {
    root_dir().join("tmp")
}

// Handles bin dir logic.
pub fn bin_dir() -> PathBuf {
    root_dir().join("bin")
}

pub fn installed_executable_path() -> PathBuf {
    bin_dir().join(format!("mlai-trade{}", std::env::consts::EXE_SUFFIX))
}

fn executable_path_is_usable(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    if path.to_string_lossy().ends_with(" (deleted)") {
        return false;
    }
    fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

pub fn command_executable_path() -> anyhow::Result<PathBuf> {
    if let Ok(current) = std::env::current_exe() {
        if executable_path_is_usable(&current) {
            return Ok(current);
        }
    }

    let installed = installed_executable_path();
    if executable_path_is_usable(&installed) {
        return Ok(installed);
    }

    let current = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("unavailable: {err}"));
    anyhow::bail!(
        "unable to locate runnable mlai-trade executable; current_exe={current}, installed={}",
        installed.display()
    )
}

// Ensures runtime dirs exists or meets required invariants.
pub fn ensure_runtime_dirs() -> anyhow::Result<()> {
    ensure_private_dir(&root_dir())?;
    for dir in [
        config_dir(),
        data_dir(),
        db_dir(),
        logs_dir(),
        archived_logs_dir(),
        api_dir(),
        tmp_dir(),
    ] {
        ensure_private_dir(&dir)?;
    }
    std::fs::create_dir_all(bin_dir())?;
    std::fs::create_dir_all(docs_dir())?;
    harden_sensitive_runtime_permissions()?;
    Ok(())
}

// Ensures state dir exists or meets required invariants.
pub fn ensure_state_dir() -> anyhow::Result<PathBuf> {
    ensure_runtime_dirs()?;
    Ok(data_dir())
}

// Handles state dir logic.
pub fn state_dir() -> PathBuf {
    data_dir()
}

// Handles legacy candidates logic.
fn legacy_candidates(current_dir: &Path, legacy_name: &str) -> Vec<PathBuf> {
    let root = root_dir();
    vec![current_dir.join(legacy_name), root.join(legacy_name)]
}

// Handles named path in logic.
fn named_path_in(dir: PathBuf, current_name: &str, legacy_names: &[&str]) -> PathBuf {
    let current = dir.join(current_name);
    if current.exists() {
        return current;
    }

    for legacy_name in legacy_names {
        for legacy in legacy_candidates(&dir, legacy_name) {
            if legacy.exists() {
                if let Some(parent) = current.parent() {
                    if let Err(err) = ensure_private_dir(parent) {
                        eprintln!("warning: could not create {}: {}", parent.display(), err);
                        return legacy;
                    }
                }
                if let Err(err) = std::fs::rename(&legacy, &current) {
                    eprintln!(
                        "warning: could not rename {} to {}: {}",
                        legacy.display(),
                        current.display(),
                        err
                    );
                    return legacy;
                }
                let _ = harden_file_if_exists(&current);
                return current;
            }
        }
    }

    current
}

// Returns the runtime path for scanner db path.
pub fn scanner_db_path() -> PathBuf {
    named_path_in(
        db_dir(),
        "mlai_trade.db",
        &["alpaca_market_research.db", "scanner.db"],
    )
}

// Returns the runtime path for ml model path.
pub fn ml_model_path() -> PathBuf {
    named_path_in(data_dir(), "lightgbm_model.txt", &["ml_model.txt"])
}

// Returns LSTM model path runtime settings.
pub fn lstm_model_path() -> PathBuf {
    named_path_in(data_dir(), "lstm_sequence_model.bin", &["lstm_model.bin"])
}

// Returns the runtime path for ml dataset csv path.
pub fn ml_dataset_csv_path() -> PathBuf {
    named_path_in(
        data_dir(),
        "ml_feature_label_export.csv",
        &["ml_dataset.csv"],
    )
}

// Returns the runtime path for lightgbm training dataset path.
pub fn lightgbm_training_dataset_path() -> PathBuf {
    named_path_in(
        data_dir(),
        "lightgbm_training_dataset.txt",
        &["lightgbm_train.txt"],
    )
}

// Returns the runtime path for lightgbm validation dataset path.
pub fn lightgbm_validation_dataset_path() -> PathBuf {
    named_path_in(
        data_dir(),
        "lightgbm_validation_dataset.txt",
        &["lightgbm_valid.txt"],
    )
}

// Returns the runtime path for lightgbm training report path.
pub fn lightgbm_training_report_path() -> PathBuf {
    named_path_in(
        data_dir(),
        "lightgbm_training_report.json",
        &["ml_backtest_results.json"],
    )
}
