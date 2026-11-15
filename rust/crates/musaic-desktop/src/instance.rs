//! Single instance per config dir.
//!
//! Two processes on one `library.db` would contend on the SQLite WAL file
//! (the reason the Electron shell took `app.requestSingleInstanceLock()`).
//! The lock is keyed on the *config dir*, not the app identifier, so a test
//! run on a temp config dir never collides with the user's real instance,
//! and `tauri-plugin-single-instance` (one `/tmp/<identifier>_si.sock` per
//! machine on macOS) is not used.
//!
//! * `<config>/desktop.lock` — an OS advisory lock (`flock` / `LockFileEx`)
//!   held for the life of the process; the OS drops it on any exit, crash
//!   included.
//! * `<config>/desktop-instance.json` — `{"pid", "port", "focus"}` written
//!   (mode 0600 on Unix) once the server is bound. A separate file because a
//!   Windows lock also blocks reads of the locked file.
//!
//! A second launch fails to take the lock, reads the info file and sends
//! `POST /__musaic/focus` with the `X-Musaic-Focus` secret; the first
//! instance un-minimises, shows and focuses its window. The second exits 0.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fd_lock::{RwLock, RwLockWriteGuard};

pub const LOCK_FILE: &str = "desktop.lock";
pub const INFO_FILE: &str = "desktop-instance.json";

pub struct InstanceLock {
    _guard: RwLockWriteGuard<'static, File>,
    info: PathBuf,
}

pub enum Acquired {
    /// This process is the instance.
    First(InstanceLock),
    /// Another process holds the lock.
    Running,
}

pub fn acquire(config_dir: &Path) -> std::io::Result<Acquired> {
    std::fs::create_dir_all(config_dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(config_dir.join(LOCK_FILE))?;
    // Leaked on purpose: the lock lives as long as the process.
    let lock: &'static mut RwLock<File> = Box::leak(Box::new(RwLock::new(file)));
    match lock.try_write() {
        Ok(guard) => Ok(Acquired::First(InstanceLock {
            _guard: guard,
            info: config_dir.join(INFO_FILE),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(Acquired::Running),
        Err(e) => Err(e),
    }
}

impl InstanceLock {
    /// Record where the running instance answers and its focus secret.
    pub fn publish(&self, port: u16, focus_secret: &str) -> std::io::Result<()> {
        let body = serde_json::json!({
            "pid": std::process::id(),
            "port": port,
            "focus": focus_secret,
        })
        .to_string();
        let tmp = self.info.with_extension("json.tmp");
        {
            let mut o = OpenOptions::new();
            o.create(true).truncate(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            let mut f = o.open(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.info)
    }

    /// Remove the info file at a clean exit (the lock goes with the process).
    pub fn retire(&self) {
        let _ = std::fs::remove_file(&self.info);
    }
}

/// Ask the running instance to focus its window. Retries until `timeout`
/// (the first instance may still be starting and not yet have published).
pub fn ask_running_to_focus(config_dir: &Path, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let res = try_focus(config_dir);
        if res.is_ok() || Instant::now() >= deadline {
            return res;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn try_focus(config_dir: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(config_dir.join(INFO_FILE)).map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let port = v["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .ok_or("bad port")?;
    let secret = v["focus"].as_str().ok_or("no focus secret")?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s =
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{}: {secret}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        crate::gate::FOCUS_PATH,
        crate::gate::FOCUS_HEADER
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut resp = String::new();
    let _ = s.read_to_string(&mut resp);
    if resp.starts_with("HTTP/1.1 204") {
        Ok(())
    } else {
        Err(format!(
            "focus refused: {}",
            resp.lines().next().unwrap_or("(no answer)")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One instance per config dir; another dir is independent.
    #[test]
    fn second_lock_on_same_dir_is_refused_and_other_dirs_are_free() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let first = acquire(a.path()).unwrap();
        assert!(matches!(first, Acquired::First(_)));
        assert!(matches!(acquire(a.path()).unwrap(), Acquired::Running));
        assert!(matches!(acquire(b.path()).unwrap(), Acquired::First(_)));
        if let Acquired::First(l) = first {
            l.publish(1234, "s3cret").unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(a.path().join(INFO_FILE)).unwrap())
                    .unwrap();
            assert_eq!(v["port"], 1234);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(a.path().join(INFO_FILE))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o077, 0, "info file must be private");
            }
            l.retire();
            assert!(!a.path().join(INFO_FILE).exists());
        }
    }
}
