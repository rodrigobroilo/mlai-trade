// JSONL component logging and daily rotation.
//
// Function map:
// - component_log_path(): resolves configured log paths into logs/.
// - append_component_event*(): writes one JSON event safely.
// - ensure_json_lines(): converts legacy plaintext lines into JSON events.
// - rotate_if_needed(): compresses yesterday's log and truncates current log.

use crate::{config, paths};
use chrono::{DateTime, Local, NaiveDate, Utc};
use flate2::{write::GzEncoder, Compression};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

// Handles non empty string logic.
fn non_empty_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

// Returns the runtime path for component log path.
pub fn component_log_path(component: &str) -> PathBuf {
    let config = config::load().ok();
    let configured = match component {
        "data" => config.and_then(|config| non_empty_string(config.logging.data_log_file)),
        "ml" => config.and_then(|config| non_empty_string(config.logging.ml_log_file)),
        "training" => config.and_then(|config| non_empty_string(config.logging.training_log_file)),
        "feeds" => config.and_then(|config| non_empty_string(config.logging.feeds_log_file)),
        other => {
            return paths::logs_dir().join(format!(
                "mlai-trade-{}.log",
                other
                    .chars()
                    .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                    .collect::<String>()
            ));
        }
    };
    paths::path_in_runtime_dir(
        paths::logs_dir(),
        configured,
        &format!("mlai-trade-{component}.log"),
    )
}

// Handles append component event logic.
pub fn append_component_event(component: &str, mut event: serde_json::Value) -> io::Result<()> {
    if let Some(object) = event.as_object_mut() {
        object.entry("ts".to_string()).or_insert_with(|| {
            serde_json::json!(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
        });
        object
            .entry("component".to_string())
            .or_insert_with(|| serde_json::json!(component));
    }
    let path = component_log_path(component);
    if let Some(parent) = path.parent() {
        paths::ensure_private_dir(parent)?;
    }
    ensure_json_lines(&path, component)?;
    rotate_if_needed(&path)?;
    let mut file = paths::open_private_append(&path)?;
    serde_json::to_writer(&mut file, &event).map_err(io::Error::other)?;
    writeln!(file)?;
    Ok(())
}

// Handles append component event lossy logic.
pub fn append_component_event_lossy(component: &str, event: serde_json::Value) {
    if let Err(err) = append_component_event(component, event) {
        let fallback = serde_json::json!({
            "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "component": component,
            "event": "component_log_write_failed",
            "level": "error",
            "error": err.to_string(),
        });
        eprintln!("{fallback}");
    }
}

// Ensures json lines exists or meets required invariants.
pub fn ensure_json_lines(path: &Path, component: &str) -> io::Result<bool> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    if metadata.len() == 0 {
        return Ok(false);
    }

    let mut needs_rewrite = false;
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || serde_json::from_str::<serde_json::Value>(trimmed).is_err() {
            needs_rewrite = true;
            break;
        }
    }
    if !needs_rewrite {
        return Ok(false);
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    paths::ensure_private_dir(parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("mlai-trade.log");
    let tmp_path = parent.join(format!(".{file_name}.jsonlines.tmp"));
    {
        let input = BufReader::new(File::open(path)?);
        let mut output = BufWriter::new(paths::create_private_file(&tmp_path)?);
        for line in input.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
                writeln!(output, "{trimmed}")?;
            } else {
                let event = serde_json::json!({
                    "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                    "component": component,
                    "event": "legacy_plaintext_log",
                    "level": "warn",
                    "message": line,
                });
                serde_json::to_writer(&mut output, &event).map_err(io::Error::other)?;
                writeln!(output)?;
            }
        }
        output.flush()?;
    }
    fs::rename(tmp_path, path)?;
    paths::harden_file_if_exists(path)?;
    Ok(true)
}

// Handles rotate if needed logic.
pub fn rotate_if_needed(path: &Path) -> io::Result<Option<PathBuf>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    if metadata.len() == 0 {
        return Ok(None);
    }

    let modified: DateTime<Local> = metadata.modified()?.into();
    let archive_date = modified.date_naive();
    if archive_date >= Local::now().date_naive() {
        return Ok(None);
    }

    rotate_with_date(path, archive_date).map(Some)
}

// Handles rotate with date logic.
pub(crate) fn rotate_with_date(path: &Path, archive_date: NaiveDate) -> io::Result<PathBuf> {
    if let Some(parent) = path.parent() {
        paths::ensure_private_dir(parent)?;
    }

    let archive = next_archive_path(path, archive_date);
    {
        let mut input = File::open(path)?;
        let output = paths::create_private_file(&archive)?;
        let mut encoder = GzEncoder::new(output, Compression::default());
        io::copy(&mut input, &mut encoder)?;
        encoder.finish()?;
    }

    OpenOptions::new().write(true).truncate(true).open(path)?;
    paths::harden_file_if_exists(path)?;
    Ok(archive)
}

// Returns the next archive path value.
fn next_archive_path(path: &Path, archive_date: NaiveDate) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("mlai-trade.log");
    let base = format!("{}-{}", archive_date.format("%Y%m%d"), file_name);
    let first = parent.join(format!("{base}.gz"));
    if !first.exists() {
        return first;
    }

    for index in 1.. {
        let candidate = parent.join(format!("{base}.{index}.gz"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("unbounded archive suffix search should always return")
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;

    #[test]
    // Handles rotate with date compresses and truncates active log logic.
    fn rotate_with_date_compresses_and_truncates_active_log() {
        let dir = std::env::temp_dir().join(format!(
            "mlai-trade-log-test-{}-{}",
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mlai-trade-auto.log");
        fs::write(&path, "one\ntwo\n").unwrap();

        let archive = rotate_with_date(&path, NaiveDate::from_ymd_opt(2026, 5, 2).unwrap())
            .expect("rotate log");

        assert_eq!(
            archive.file_name().and_then(|value| value.to_str()),
            Some("20260502-mlai-trade-auto.log.gz")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "");

        let mut decoded = String::new();
        GzDecoder::new(File::open(&archive).unwrap())
            .read_to_string(&mut decoded)
            .unwrap();
        assert_eq!(decoded, "one\ntwo\n");

        let _ = fs::remove_dir_all(&dir);
    }
}
