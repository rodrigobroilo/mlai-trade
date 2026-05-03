use std::path::{Path, PathBuf};

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(dirs::home_dir)
        .expect("unable to determine home directory; set MLAI_TRADE_HOME explicitly")
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

pub fn root_dir() -> PathBuf {
    std::env::var("MLAI_TRADE_HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| expand_tilde(value.trim()))
        .unwrap_or_else(|| home_dir().join("mlai-trade"))
}

pub fn data_dir() -> PathBuf {
    root_dir().join("data")
}

pub fn db_dir() -> PathBuf {
    root_dir().join("db")
}

pub fn config_dir() -> PathBuf {
    root_dir().join("config")
}

pub fn docs_dir() -> PathBuf {
    root_dir().join("docs")
}

pub fn logs_dir() -> PathBuf {
    root_dir().join("logs")
}

pub fn api_dir() -> PathBuf {
    root_dir().join("api")
}

pub fn tmp_dir() -> PathBuf {
    root_dir().join("tmp")
}

pub fn bin_dir() -> PathBuf {
    root_dir().join("bin")
}

pub fn ensure_runtime_dirs() -> anyhow::Result<()> {
    std::fs::create_dir_all(root_dir())?;
    for dir in [
        bin_dir(),
        config_dir(),
        data_dir(),
        db_dir(),
        docs_dir(),
        logs_dir(),
        api_dir(),
        tmp_dir(),
    ] {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

pub fn ensure_state_dir() -> anyhow::Result<PathBuf> {
    ensure_runtime_dirs()?;
    Ok(data_dir())
}

pub fn state_dir() -> PathBuf {
    data_dir()
}

fn legacy_candidates(current_dir: &Path, legacy_name: &str) -> Vec<PathBuf> {
    let root = root_dir();
    vec![current_dir.join(legacy_name), root.join(legacy_name)]
}

fn named_path_in(dir: PathBuf, current_name: &str, legacy_names: &[&str]) -> PathBuf {
    let current = dir.join(current_name);
    if current.exists() {
        return current;
    }

    for legacy_name in legacy_names {
        for legacy in legacy_candidates(&dir, legacy_name) {
            if legacy.exists() {
                if let Some(parent) = current.parent() {
                    if let Err(err) = std::fs::create_dir_all(parent) {
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
                return current;
            }
        }
    }

    current
}

pub fn scanner_db_path() -> PathBuf {
    named_path_in(
        db_dir(),
        "mlai_trade.db",
        &["alpaca_market_research.db", "scanner.db"],
    )
}

pub fn ml_model_path() -> PathBuf {
    named_path_in(data_dir(), "lightgbm_model.txt", &["ml_model.txt"])
}

pub fn lstm_model_path() -> PathBuf {
    named_path_in(data_dir(), "lstm_sequence_model.bin", &["lstm_model.bin"])
}

pub fn ml_dataset_csv_path() -> PathBuf {
    named_path_in(
        data_dir(),
        "ml_feature_label_export.csv",
        &["ml_dataset.csv"],
    )
}

pub fn lightgbm_training_dataset_path() -> PathBuf {
    named_path_in(
        data_dir(),
        "lightgbm_training_dataset.txt",
        &["lightgbm_train.txt"],
    )
}

pub fn lightgbm_validation_dataset_path() -> PathBuf {
    named_path_in(
        data_dir(),
        "lightgbm_validation_dataset.txt",
        &["lightgbm_valid.txt"],
    )
}

pub fn lightgbm_training_report_path() -> PathBuf {
    named_path_in(
        data_dir(),
        "lightgbm_training_report.json",
        &["ml_backtest_results.json"],
    )
}
