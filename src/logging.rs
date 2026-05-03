use chrono::{DateTime, Local, NaiveDate};
use flate2::{write::GzEncoder, Compression};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

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

pub(crate) fn rotate_with_date(path: &Path, archive_date: NaiveDate) -> io::Result<PathBuf> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let archive = next_archive_path(path, archive_date);
    {
        let mut input = File::open(path)?;
        let output = File::create(&archive)?;
        let mut encoder = GzEncoder::new(output, Compression::default());
        io::copy(&mut input, &mut encoder)?;
        encoder.finish()?;
    }

    OpenOptions::new().write(true).truncate(true).open(path)?;
    Ok(archive)
}

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
