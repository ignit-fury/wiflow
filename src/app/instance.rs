//! Single-instance guard. Two running copies would both hear the hotkey and
//! paste every dictation twice (easy to hit: the login item plus a manual
//! launch of Wiflow.app). The first copy listens on a Unix socket; a later
//! launch connects to it, which asks the first copy to show its window, and
//! then exits.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

/// True when another copy answered on the socket (it now shows its window).
pub fn already_running(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

/// Become the running copy: call `on_launch` each time a later launch
/// connects. On failure Wiflow still runs, just without the guard.
pub fn listen(socket: &Path, on_launch: impl Fn() + Send + 'static) {
    // A socket file left by a crashed copy refuses connections; replace it.
    let _ = std::fs::remove_file(socket);
    if let Some(dir) = socket.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match UnixListener::bind(socket) {
        Ok(listener) => {
            std::thread::spawn(move || {
                for _ in listener.incoming().flatten() {
                    on_launch();
                }
            });
        }
        Err(e) => tracing::warn!("single-instance guard off: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_launch_is_detected_and_signals_the_first() {
        let socket =
            std::env::temp_dir().join(format!("wiflow_instance_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        assert!(!already_running(&socket), "nothing listening yet");

        // A stale file from a crashed copy must not count as running.
        std::fs::write(&socket, "").unwrap();
        assert!(!already_running(&socket));

        let (tx, rx) = std::sync::mpsc::channel();
        listen(&socket, move || tx.send(()).unwrap());
        assert!(already_running(&socket));
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("first copy was told about the launch");
        std::fs::remove_file(&socket).unwrap();
    }
}
