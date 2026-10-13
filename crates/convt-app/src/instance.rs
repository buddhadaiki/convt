//! One app per user. The first launch holds a lock and listens on a local
//! socket; later launches send their request there and exit.
//!
//! The socket lives in a directory only this user can open. The protocol is
//! one JSON [`Request`] per connection, answered with `ok`.

use std::path::PathBuf;

#[cfg(windows)]
use crate::request::Request;

/// Requests larger than this are dropped. A request is a list of paths.
#[cfg(unix)]
const MAX_REQUEST: u64 = 4 << 20;

pub enum Role {
    /// This process is the app. Call [`Primary::listen`] once the UI can
    /// take requests.
    Primary(Primary),
    /// The running app took the request.
    #[cfg_attr(not(unix), allow(dead_code))]
    Forwarded,
}

/// `$CONVT_RUNTIME_DIR`, else `$XDG_RUNTIME_DIR/convt`, else a per-user
/// folder in the temp directory.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CONVT_RUNTIME_DIR") {
        return dir.into();
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("convt");
    }
    #[cfg(unix)]
    let name = format!("convt-{}", unsafe { libc::getuid() });
    #[cfg(not(unix))]
    let name = "convt".to_string();
    std::env::temp_dir().join(name)
}

#[cfg(unix)]
pub use unix::{Primary, claim};

#[cfg(unix)]
mod unix {
    use std::fs::{DirBuilder, File, OpenOptions};
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::{MAX_REQUEST, Role};
    use crate::request::Request;

    /// How long a second launch waits for a starting app to bind its socket.
    const CONNECT_WAIT: Duration = Duration::from_secs(5);

    pub struct Primary {
        listener: UnixListener,
        socket: PathBuf,
        /// Held for the life of the process; releasing it lets a new app start.
        _lock: File,
    }

    impl Primary {
        /// Accepts requests on a background thread and hands each to `on_request`.
        pub fn listen(self, on_request: impl Fn(Request) + Send + 'static) {
            std::thread::Builder::new()
                .name("convt-instance".into())
                .spawn(move || {
                    let Primary {
                        listener,
                        socket,
                        _lock,
                    } = self;
                    for stream in listener.incoming() {
                        match stream.and_then(read_request) {
                            Ok(req) => on_request(req),
                            Err(e) => tracing::warn!(error = %e, socket = %socket.display(), "bad request"),
                        }
                    }
                })
                .expect("spawn instance listener");
        }
    }

    fn read_request(stream: UnixStream) -> io::Result<Request> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut line = Vec::new();
        BufReader::new((&stream).take(MAX_REQUEST)).read_until(b'\n', &mut line)?;
        let req = serde_json::from_slice(&line).map_err(io::Error::other)?;
        (&stream).write_all(b"ok\n")?;
        Ok(req)
    }

    fn send(stream: UnixStream, req: &Request) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut line = serde_json::to_vec(req).map_err(io::Error::other)?;
        line.push(b'\n');
        (&stream).write_all(&line)?;
        let mut reply = String::new();
        BufReader::new(&stream).read_line(&mut reply)?;
        if reply.trim() == "ok" {
            Ok(())
        } else {
            Err(io::Error::other(
                "the running app did not accept the request",
            ))
        }
    }

    /// Creates `dir` for this user only, and refuses one that someone else
    /// owns or can open.
    fn private_dir(dir: &Path) -> io::Result<()> {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let meta = std::fs::metadata(dir)?;
        let uid = unsafe { libc::getuid() };
        if meta.uid() != uid || meta.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} must belong to you and be private (mode 700)",
                    dir.display()
                ),
            ));
        }
        Ok(())
    }

    /// Becomes the app, or hands `req` to the one already running.
    pub fn claim(dir: &Path, req: &Request) -> io::Result<Role> {
        private_dir(dir)?;
        let socket = dir.join("app.sock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(dir.join("app.lock"))?;
        // SAFETY: flock on a file descriptor we own.
        let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if locked {
            // Whoever held the lock before is gone; its socket is stale.
            match std::fs::remove_file(&socket) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            let listener = UnixListener::bind(&socket)?;
            return Ok(Role::Primary(Primary {
                listener,
                socket,
                _lock: lock,
            }));
        }
        // Another app holds the lock. It may still be starting, so retry
        // until its socket answers.
        let start = Instant::now();
        loop {
            match UnixStream::connect(&socket) {
                Ok(stream) => {
                    send(stream, req)?;
                    return Ok(Role::Forwarded);
                }
                Err(e) if start.elapsed() > CONNECT_WAIT => {
                    return Err(io::Error::new(
                        e.kind(),
                        format!(
                            "convt is running but not answering at {}: {e}",
                            socket.display()
                        ),
                    ));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

#[cfg(windows)]
pub struct Primary {
    /// Held, never read: owning the named mutex is what makes this the primary.
    #[allow(dead_code)]
    mutex: windows_sys::Win32::Foundation::HANDLE,
    dir: PathBuf,
}

#[cfg(windows)]
impl Primary {
    pub fn listen(self, on_request: impl Fn(Request) + Send + 'static) {
        // Keep the mutex for the life of the process so later launches forward.
        let dir = self.dir.clone();
        std::mem::forget(self);
        std::thread::Builder::new()
            .name("convt-instance".into())
            .spawn(move || {
                loop {
                    drain_inbox(&dir, &on_request);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            })
            .expect("spawn instance listener");
    }
}

#[cfg(windows)]
pub(crate) fn drain_inbox(dir: &std::path::Path, on_request: impl Fn(Request)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut requests: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("request-") && name.ends_with(".json"))
        })
        .collect();
    requests.sort();
    for path in requests {
        let bytes = std::fs::read(&path);
        let _ = std::fs::remove_file(&path);
        match bytes.and_then(|bytes| serde_json::from_slice(&bytes).map_err(std::io::Error::other))
        {
            Ok(request) => on_request(request),
            Err(error) => {
                tracing::warn!(error = %error, path = %path.display(), "bad instance request")
            }
        }
    }
}

/// Keep the production namespace unchanged, but isolate a caller's explicit
/// runtime directory. The Explorer handler uses this same UTF-16 FNV-1a key.
#[cfg(windows)]
fn mutex_name(runtime_override: Option<&std::path::Path>) -> String {
    use std::os::windows::ffi::OsStrExt;
    match runtime_override {
        None => "Local\\convt-instance".into(),
        Some(dir) => {
            let key = dir
                .as_os_str()
                .encode_wide()
                .fold(14695981039346656037u64, |key, unit| {
                    (key ^ u64::from(unit)).wrapping_mul(1099511628211)
                });
            format!("Local\\convt-instance-{key:016x}")
        }
    }
}

#[cfg(windows)]
fn claim_windows(dir: &std::path::Path, req: Option<&Request>) -> std::io::Result<Role> {
    let runtime_override = std::env::var_os("CONVT_RUNTIME_DIR").map(PathBuf::from);
    claim_named(dir, req, &mutex_name(runtime_override.as_deref()))
}

#[cfg(windows)]
pub fn claim(dir: &std::path::Path, req: &Request) -> std::io::Result<Role> {
    claim_windows(dir, Some(req))
}

/// Explorer queued its request before launching. If another process wins
/// startup, its listener already owns that request; do not enqueue a second
/// empty request that would open Activity in that process.
#[cfg(windows)]
pub fn claim_queued(dir: &std::path::Path) -> std::io::Result<Role> {
    claim_windows(dir, None)
}

#[cfg(windows)]
fn claim_named(dir: &std::path::Path, req: Option<&Request>, name: &str) -> std::io::Result<Role> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    std::fs::create_dir_all(dir)?;
    // The Local namespace is scoped to the interactive user's session. A
    // machine-wide mutex would make one user's launch suppress another user's
    // independent app and request inbox.
    let name: Vec<u16> = std::ffi::OsStr::new(name)
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: the name is a valid NUL-terminated string and no initial owner
    // is requested; the handle is retained until the primary exits.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if mutex.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(mutex) };
        let Some(req) = req else {
            return Ok(Role::Forwarded);
        };
        let bytes = serde_json::to_vec(req).map_err(std::io::Error::other)?;
        let id = format!(
            "{}-{}",
            std::process::id(),
            REQUEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let temp = dir.join(format!("request-{id}.tmp"));
        let path = dir.join(format!("request-{id}.json"));
        std::fs::write(&temp, bytes)?;
        std::fs::rename(temp, path)?;
        return Ok(Role::Forwarded);
    }
    Ok(Role::Primary(Primary {
        mutex,
        dir: dir.into(),
    }))
}

#[cfg(windows)]
static REQUEST_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(windows)]
impl Drop for Primary {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.mutex) };
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::cell::RefCell;
    use std::path::Path;

    use super::*;

    #[test]
    fn runtime_override_uses_the_same_stable_namespace_as_explorer() {
        assert_eq!(mutex_name(None), "Local\\convt-instance");
        assert_eq!(
            mutex_name(Some(Path::new(r"C:\convt-runtime-ação"))),
            "Local\\convt-instance-bafbcab710422a4c"
        );
        assert_ne!(
            mutex_name(Some(Path::new(r"C:\one"))),
            mutex_name(Some(Path::new(r"C:\two")))
        );
    }

    #[test]
    fn queued_second_launch_does_not_add_a_main_window_request() {
        let dir = tempfile::tempdir().unwrap();
        let name = mutex_name(Some(dir.path()));
        let request = Request {
            files: vec![dir.path().join("ação & file.png")],
            source: Some(crate::request::Source::Cli),
            ..Request::default()
        };
        std::fs::write(
            dir.path().join("request-explorer.json"),
            serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        let Role::Primary(primary) = claim_named(dir.path(), None, &name).unwrap() else {
            panic!("first queued launch must own the instance");
        };
        assert!(matches!(
            claim_named(dir.path(), None, &name).unwrap(),
            Role::Forwarded
        ));
        let received = RefCell::new(Vec::new());
        drain_inbox(dir.path(), |request| received.borrow_mut().push(request));
        assert_eq!(received.into_inner(), [request]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        drop(primary);
        assert!(matches!(
            claim_named(dir.path(), None, &name).unwrap(),
            Role::Primary(_)
        ));
    }

    #[test]
    fn normal_second_launch_still_forwards_its_explicit_request_once() {
        let dir = tempfile::tempdir().unwrap();
        let name = mutex_name(Some(dir.path()));
        let Role::Primary(_primary) = claim_named(dir.path(), None, &name).unwrap() else {
            panic!("first launch must own the instance");
        };
        let request = Request {
            files: vec![dir.path().join("photo.png")],
            to: Some("webp".into()),
            source: Some(crate::request::Source::Cli),
            ..Request::default()
        };
        assert!(matches!(
            claim_named(dir.path(), Some(&request), &name).unwrap(),
            Role::Forwarded
        ));
        let received = RefCell::new(Vec::new());
        drain_inbox(dir.path(), |request| received.borrow_mut().push(request));
        assert_eq!(received.into_inner(), [request]);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::request::Request;

    use super::*;

    #[test]
    fn second_launch_forwards_to_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join("rt");
        let Role::Primary(primary) = claim(&rt, &Request::default()).unwrap() else {
            panic!("first launch should be primary");
        };
        let (tx, rx) = mpsc::channel();
        primary.listen(move |r| tx.send(r).unwrap());
        let req = Request {
            files: vec![PathBuf::from("/tmp/a b \"c\".png")],
            to: Some("jpeg".into()),
            ..Request::default()
        };
        assert!(matches!(claim(&rt, &req).unwrap(), Role::Forwarded));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), req);
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join("rt");
        {
            let Role::Primary(_gone) = claim(&rt, &Request::default()).unwrap() else {
                panic!()
            };
        }
        // The socket file is left behind, but the lock is free again.
        assert!(rt.join("app.sock").exists());
        assert!(matches!(
            claim(&rt, &Request::default()).unwrap(),
            Role::Primary(_)
        ));
    }

    #[test]
    fn refuses_a_shared_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(claim(dir.path(), &Request::default()).is_err());
    }
}
