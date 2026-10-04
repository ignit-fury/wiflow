//! File logging for the bundled app: Finder-launched apps have no terminal,
//! so all tracing output tees to `~/Library/Logs/wiflow/wiflow.log` as well
//! as stdout (terminal runs keep working unchanged). Rotated past 10 MB.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const MAX_BYTES: u64 = 10_000_000;

pub fn log_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Logs/wiflow/wiflow.log")
}

/// Move aside an oversized log (previous `.old` discarded).
fn rotate(path: &std::path::Path) {
    rotate_sized(path, MAX_BYTES);
}

fn rotate_sized(path: &std::path::Path, max: u64) {
    if std::fs::metadata(path)
        .map(|m| m.len() > max)
        .unwrap_or(false)
    {
        let old = path.with_extension("log.old");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(path, &old);
    }
}

#[derive(Clone, Default)]
struct Tee(Option<Arc<Mutex<std::fs::File>>>);

struct TeeWriter {
    file: Option<Arc<Mutex<std::fs::File>>>,
}

impl std::io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stdout().write_all(buf);
        if let Some(f) = &self.file {
            if let Ok(mut g) = f.lock() {
                let _ = g.write_all(buf);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stdout().flush();
        if let Some(f) = &self.file {
            if let Ok(mut g) = f.lock() {
                let _ = g.flush();
            }
        }
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Tee {
    type Writer = TeeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TeeWriter {
            file: self.0.clone(),
        }
    }
}

/// Install the global subscriber: INFO to both stdout and the log file.
/// File failures degrade to stdout-only (never crash startup).
pub fn init() {
    let path = log_path();
    let file = path
        .parent()
        .map(std::fs::create_dir_all)
        .map(|r| r.ok())
        .and_then(|_| {
            rotate(&path);
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .ok()
        })
        .map(|f| Arc::new(Mutex::new(f)));
    if file.is_some() {
        eprintln!("wiflow log: {}", path.display());
    }
    tracing_subscriber::fmt()
        .with_writer(Tee(file))
        .with_max_level(tracing::Level::INFO)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_path_name() {
        assert_eq!(
            log_path().file_name().unwrap().to_string_lossy(),
            "wiflow.log"
        );
    }

    #[test]
    fn test_rotate_moves_oversized() {
        let dir = std::env::temp_dir().join("wiflow_log_test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("wiflow.log");
        std::fs::write(&p, vec![b'x'; 100]).unwrap();
        rotate_sized(&p, 10_000_000);
        assert!(p.exists(), "under limit: untouched");
        rotate_sized(&p, 10);
        let old = p.with_extension("log.old");
        assert!(!p.exists(), "over limit: moved");
        assert_eq!(std::fs::metadata(&old).unwrap().len(), 100);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tee_writer_goes_to_file() {
        let dir = std::env::temp_dir().join("wiflow_tee_test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("t.log");
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .unwrap();
        let tee = Tee(Some(Arc::new(Mutex::new(f))));
        let mut w = <Tee as tracing_subscriber::fmt::MakeWriter>::make_writer(&tee);
        use std::io::Write as _;
        w.write_all(b"hello-log").unwrap();
        w.flush().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello-log");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
