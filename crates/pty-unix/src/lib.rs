//! PTY Unix con un solo hilo IO (`mio`) para todos los paneles.

use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token, Waker};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::Arc;
use termust_core::pty::{PtyError, PtyEvent, PtyHandle, PtySession, PtySpawner, SpawnSpec};
use termust_core::terminal::Size;

const WAKE_TOKEN: Token = Token(0);
const READ_BUF: usize = 65536;
const EVENT_QUEUE: usize = 64;

enum HubCmd {
    Add {
        id: u64,
        file: File,
        child: Child,
        tx: SyncSender<PtyEvent>,
    },
    Write {
        id: u64,
        bytes: Vec<u8>,
    },
    Resize {
        id: u64,
        size: Size,
    },
    Kill {
        id: u64,
    },
}

struct Entry {
    file: File,
    child: Child,
    tx: SyncSender<PtyEvent>,
}

pub struct UnixPtySpawner {
    tx: Sender<HubCmd>,
    waker: Arc<Waker>,
    next: AtomicU64,
}

impl UnixPtySpawner {
    pub fn new() -> Result<Self, PtyError> {
        let poll = Poll::new().map_err(|e| PtyError::Io(e.to_string()))?;
        let waker = Arc::new(
            Waker::new(poll.registry(), WAKE_TOKEN).map_err(|e| PtyError::Io(e.to_string()))?,
        );
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("termust-pty-io".into())
            .spawn(move || hub_loop(poll, rx))
            .map_err(|e| PtyError::Spawn(e.to_string()))?;
        Ok(Self {
            tx,
            waker,
            next: AtomicU64::new(1),
        })
    }
}

impl PtySpawner for UnixPtySpawner {
    fn spawn(&self, spec: SpawnSpec) -> Result<PtyHandle, PtyError> {
        if spec.argv.is_empty() {
            return Err(PtyError::Spawn("empty argv".into()));
        }
        if spec.size.cols == 0 || spec.size.rows == 0 {
            return Err(PtyError::Spawn("invalid size".into()));
        }

        let ws = libc::winsize {
            ws_row: spec.size.rows as u16,
            ws_col: spec.size.cols as u16,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // O_CLOEXEC atómico: un fork concurrente de otra pestaña no puede
        // heredar este master.
        let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        if master < 0 {
            return Err(PtyError::Io(std::io::Error::last_os_error().to_string()));
        }
        let setup = unsafe {
            if libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
                Err(std::io::Error::last_os_error())
            } else {
                let mut name = [0 as libc::c_char; 128];
                if libc::ptsname_r(master, name.as_mut_ptr(), name.len()) != 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    let slave = libc::open(
                        name.as_ptr(),
                        libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
                    );
                    if slave < 0 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        libc::ioctl(master, libc::TIOCSWINSZ, &ws);
                        let flags = libc::fcntl(master, libc::F_GETFL);
                        if flags >= 0 {
                            libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
                        }
                        Ok(slave)
                    }
                }
            }
        };
        let slave = match setup {
            Ok(slave) => slave,
            Err(e) => {
                unsafe {
                    libc::close(master);
                }
                return Err(PtyError::Io(e.to_string()));
            }
        };
        let query_fd = unsafe { libc::dup(master) };
        if query_fd >= 0 {
            unsafe {
                let flags = libc::fcntl(query_fd, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(query_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                }
            }
        }
        let master_file = unsafe { File::from_raw_fd(master) };
        let slave_file = unsafe { File::from_raw_fd(slave) };

        let mut cmd = Command::new(&spec.argv[0]);
        cmd.args(&spec.argv[1..]);
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::from(
            slave_file
                .try_clone()
                .map_err(|e| PtyError::Io(e.to_string()))?,
        ));
        cmd.stdout(Stdio::from(
            slave_file
                .try_clone()
                .map_err(|e| PtyError::Io(e.to_string()))?,
        ));
        cmd.stderr(Stdio::from(slave_file));

        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = cmd.spawn().map_err(|e| PtyError::Spawn(e.to_string()))?;
        let pid = child.id();
        let (tx, rx) = sync_channel(EVENT_QUEUE);
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.tx
            .send(HubCmd::Add {
                id,
                file: master_file,
                child,
                tx,
            })
            .map_err(|_| PtyError::Io("pty hub stopped".into()))?;
        let _ = self.waker.wake();

        Ok(PtyHandle {
            session: Box::new(UnixPtySession {
                id,
                tx: self.tx.clone(),
                pid,
                query_fd,
                waker: Arc::clone(&self.waker),
            }),
            events: rx,
        })
    }
}

struct UnixPtySession {
    id: u64,
    tx: Sender<HubCmd>,
    pid: u32,
    query_fd: i32,
    waker: Arc<Waker>,
}

impl PtySession for UnixPtySession {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let sent = self
            .tx
            .send(HubCmd::Write {
                id: self.id,
                bytes: bytes.to_vec(),
            })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty hub stopped"));
        let _ = self.waker.wake();
        sent
    }

    fn resize(&mut self, size: Size) -> std::io::Result<()> {
        let sent = self
            .tx
            .send(HubCmd::Resize { id: self.id, size })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty hub stopped"));
        let _ = self.waker.wake();
        sent
    }

    fn kill(&mut self) -> std::io::Result<()> {
        let sent = self
            .tx
            .send(HubCmd::Kill { id: self.id })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty hub stopped"));
        let _ = self.waker.wake();
        sent
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    fn has_foreground_job(&self) -> bool {
        if self.query_fd < 0 {
            return false;
        }
        let fg = unsafe { libc::tcgetpgrp(self.query_fd) };
        fg > 0 && fg != self.pid as i32
    }

    fn foreground_command(&self) -> Option<String> {
        if self.query_fd < 0 {
            return None;
        }
        let pgid = unsafe { libc::tcgetpgrp(self.query_fd) };
        if pgid <= 0 || pgid == self.pid as i32 {
            return None;
        }
        let bytes = std::fs::read(format!("/proc/{pgid}/cmdline")).ok()?;
        if bytes.is_empty() {
            return None;
        }
        let args: Vec<String> = bytes
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).to_string())
            .collect();
        if args.is_empty() {
            None
        } else {
            Some(args.join(" "))
        }
    }
}

impl Drop for UnixPtySession {
    fn drop(&mut self) {
        let _ = self.tx.send(HubCmd::Kill { id: self.id });
        let _ = self.waker.wake();
        if self.query_fd >= 0 {
            unsafe {
                libc::close(self.query_fd);
            }
        }
    }
}

fn hub_loop(mut poll: Poll, rx: Receiver<HubCmd>) {
    let mut events = Events::with_capacity(1024);
    let mut entries: HashMap<usize, Entry> = HashMap::new();

    loop {
        if let Err(e) = poll.poll(&mut events, Some(std::time::Duration::from_millis(500))) {
            if e.kind() != std::io::ErrorKind::Interrupted {
                eprintln!("termust-pty: poll error: {e}");
                return;
            }
        }

        while let Ok(cmd) = rx.try_recv() {
            if !handle_cmd(&mut poll, &mut entries, cmd) {
                return;
            }
        }

        for ev in &events {
            if ev.token() == WAKE_TOKEN {
                continue;
            }
            let token = ev.token().0;
            if !entries.contains_key(&token) {
                continue;
            }
            if ev.is_readable() || ev.is_read_closed() || ev.is_error() {
                read_entry(&mut poll, &mut entries, token);
            }
        }

        reap_exited(&mut poll, &mut entries);
    }
}

fn reap_exited(poll: &mut Poll, entries: &mut HashMap<usize, Entry>) {
    let mut done = Vec::new();
    for (id, entry) in entries.iter_mut() {
        match entry.child.try_wait() {
            Ok(Some(status)) => done.push((*id, status.code())),
            Ok(None) => {}
            Err(_) => done.push((*id, None)),
        }
    }
    for (id, code) in done {
        close_entry(poll, entries, id, false, Some(code));
    }
}

/// Devuelve false si el hub debe terminar.
fn handle_cmd(poll: &mut Poll, entries: &mut HashMap<usize, Entry>, cmd: HubCmd) -> bool {
    match cmd {
        HubCmd::Add {
            id,
            file,
            child,
            tx,
        } => {
            let token = Token(id as usize);
            let fd = file.as_raw_fd();
            let mut source = SourceFd(&fd);
            if poll
                .registry()
                .register(&mut source, token, Interest::READABLE)
                .is_ok()
            {
                entries.insert(id as usize, Entry { file, child, tx });
                // Drain output/EOF that may have arrived before registration.
                read_entry(poll, entries, id as usize);
            } else {
                let _ = tx.send(PtyEvent::Error("register failed".into()));
            }
        }
        HubCmd::Write { id, bytes } => {
            if let Some(e) = entries.get_mut(&(id as usize)) {
                if let Err(err) = e.file.write_all(&bytes) {
                    let _ = e.tx.send(PtyEvent::Error(err.to_string()));
                }
            }
        }
        HubCmd::Resize { id, size } => {
            if let Some(e) = entries.get(&(id as usize)) {
                set_winsize(e.file.as_raw_fd(), size);
            }
        }
        HubCmd::Kill { id } => {
            close_entry(poll, entries, id as usize, true, None);
        }
    }
    true
}

fn read_entry(poll: &mut Poll, entries: &mut HashMap<usize, Entry>, token: usize) {
    let mut buf = [0u8; READ_BUF];
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut close: Option<bool> = None;
    let mut error: Option<String> = None;

    {
        let Some(entry) = entries.get_mut(&token) else {
            return;
        };
        // Drain until WouldBlock: mio es edge-triggered y un EOF puede venir
        // en el mismo evento que los datos; si no se drena, se pierde el HUP.
        for _ in 0..16 {
            match entry.file.read(&mut buf) {
                Ok(0) => {
                    close = Some(false);
                    break;
                }
                Ok(n) => chunks.push(buf[..n].to_vec()),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::Interrupted =>
                {
                    break;
                }
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                    close = Some(false);
                    break;
                }
                Err(e) => {
                    error = Some(e.to_string());
                    close = Some(true);
                    break;
                }
            }
        }
    }

    for chunk in chunks {
        let failed = entries
            .get_mut(&token)
            .map(|e| e.tx.send(PtyEvent::Output(chunk)).is_err())
            .unwrap_or(true);
        if failed {
            close = Some(true);
            break;
        }
    }

    if let Some(msg) = error {
        if let Some(e) = entries.get(&token) {
            let _ = e.tx.send(PtyEvent::Error(msg));
        }
    }

    if let Some(kill) = close {
        close_entry(poll, entries, token, kill, None);
    }
}

fn close_entry(
    poll: &mut Poll,
    entries: &mut HashMap<usize, Entry>,
    token: usize,
    kill: bool,
    known: Option<Option<i32>>,
) {
    let Some(mut entry) = entries.remove(&token) else {
        return;
    };
    let fd = entry.file.as_raw_fd();
    let mut source = SourceFd(&fd);
    let _ = poll.registry().deregister(&mut source);
    if kill {
        let _ = entry.child.kill();
    }
    let code = match known {
        Some(code) => code,
        None => entry.child.wait().ok().and_then(|s| s.code()),
    };
    let _ = entry.tx.send(PtyEvent::Exited(code));
}

fn set_winsize(fd: RawFd, size: Size) {
    let ws = libc::winsize {
        ws_row: size.rows as u16,
        ws_col: size.cols as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::time::{Duration, Instant};

    fn spec(argv: &[&str]) -> SpawnSpec {
        SpawnSpec {
            argv: argv.iter().map(OsString::from).collect(),
            cwd: None,
            env: Vec::new(),
            size: Size::new(80, 24),
        }
    }

    fn collect(handle: &mut PtyHandle, until_exit: bool) -> (String, Option<i32>) {
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match handle.events.recv_timeout(left) {
                Ok(PtyEvent::Output(bytes)) => out.extend_from_slice(&bytes),
                Ok(PtyEvent::Exited(code)) => return (String::from_utf8_lossy(&out).into(), code),
                Ok(PtyEvent::Error(e)) => panic!("pty error: {e}"),
                Err(_) => panic!("timeout waiting for pty; exit_only={until_exit}"),
            }
        }
    }

    #[test]
    fn echo_output_and_exit_code() {
        let spawner = UnixPtySpawner::new().unwrap();
        let mut handle = spawner.spawn(spec(&["/bin/echo", "hello"])).unwrap();
        let (out, code) = collect(&mut handle, true);
        assert!(out.contains("hello"), "output: {out:?}");
        assert_eq!(code, Some(0));
    }

    #[test]
    fn write_reaches_child() {
        let spawner = UnixPtySpawner::new().unwrap();
        let mut handle = spawner.spawn(spec(&["/bin/cat"])).unwrap();
        handle.session.write(b"ping\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut out = Vec::new();
        while Instant::now() < deadline {
            match handle.events.recv_timeout(Duration::from_millis(200)) {
                Ok(PtyEvent::Output(b)) => {
                    out.extend_from_slice(&b);
                    if String::from_utf8_lossy(&out).contains("ping") {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        assert!(String::from_utf8_lossy(&out).contains("ping"));
        handle.session.kill().unwrap();
    }

    #[test]
    fn ctrl_c_interrupts_foreground_process() {
        let spawner = UnixPtySpawner::new().unwrap();
        let mut handle = spawner
            .spawn(spec(&[
                "/bin/sh",
                "-c",
                "trap 'echo GOTINT; exit 0' INT; sleep 30",
            ]))
            .unwrap();
        std::thread::sleep(Duration::from_millis(400));
        handle.session.write(&[3]).unwrap();
        let (out, code) = collect(&mut handle, true);
        assert!(out.contains("GOTINT"), "out={out:?} code={code:?}");
    }

    #[test]
    fn exit_code_propagates() {
        let spawner = UnixPtySpawner::new().unwrap();
        let mut handle = spawner.spawn(spec(&["/bin/sh", "-c", "exit 7"])).unwrap();
        let (_, code) = collect(&mut handle, true);
        assert_eq!(code, Some(7));
    }

    fn count_ptmx() -> usize {
        std::fs::read_dir("/proc/self/fd")
            .map(|dir| {
                dir.flatten()
                    .filter(|e| {
                        std::fs::read_link(e.path())
                            .map(|p| p.to_string_lossy().contains("ptmx"))
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn child_does_not_inherit_the_master_fd() {
        // El proceso de test puede traer ptmx heredados del entorno; lo que
        // importa es que el master de ESTE spawner no se sume a esa base.
        let baseline = count_ptmx();
        let dir = std::env::temp_dir().join(format!("termust-fds-{}", std::process::id()));
        let file = dir.join("fds.txt");
        let _ = std::fs::create_dir_all(&dir);
        let script = format!("ls -l /proc/self/fd > {} 2>&1", file.display());
        let spawner = UnixPtySpawner::new().unwrap();
        let mut handle = spawner.spawn(spec(&["/bin/sh", "-c", &script])).unwrap();
        let _ = collect(&mut handle, true);
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let child = text.matches("ptmx").count();
        assert!(
            child <= baseline,
            "el hijo heredó el master (baseline {baseline}, hijo {child}):\n{text}"
        );
    }

    #[test]
    fn resize_is_accepted() {
        let spawner = UnixPtySpawner::new().unwrap();
        let mut handle = spawner.spawn(spec(&["/bin/cat"])).unwrap();
        handle.session.resize(Size::new(100, 30)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        handle.session.kill().unwrap();
    }

    #[test]
    fn detects_foreground_job() {
        let spawner = UnixPtySpawner::new().unwrap();
        let mut handle = spawner
            .spawn(spec(&["/bin/bash", "--noprofile", "--norc", "-i"]))
            .unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(!handle.session.has_foreground_job());
        handle.session.write(b"sleep 5\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !handle.session.has_foreground_job() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(handle.session.has_foreground_job(), "no detectó sleep");
        handle.session.write(&[3]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && handle.session.has_foreground_job() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !handle.session.has_foreground_job(),
            "Ctrl+C no volvió al shell"
        );
        handle.session.kill().unwrap();
    }

    #[test]
    fn kill_reaps_child() {
        let spawner = UnixPtySpawner::new().unwrap();
        let mut handle = spawner.spawn(spec(&["/bin/cat"])).unwrap();
        handle.session.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match handle.events.recv_timeout(left) {
                Ok(PtyEvent::Exited(_)) => break,
                Ok(_) => {}
                Err(_) => panic!("child was not reaped"),
            }
        }
    }
}
