//! Pruebas de punta a punta del puente de PTY del host.

use std::fs::File;
use std::os::unix::io::FromRawFd;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Harness {
    master: i32,
    child: Child,
}

impl Harness {
    fn spawn(args: &[&str]) -> Self {
        let (master, slave) = open_pty();
        set_winsize(master, 24, 80);
        let s1 = unsafe { File::from_raw_fd(slave) };
        let s2 = s1.try_clone().unwrap();
        let s3 = s1.try_clone().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_termust-host-pty"))
            .args(args)
            .stdin(Stdio::from(s1))
            .stdout(Stdio::from(s2))
            .stderr(Stdio::from(s3))
            .spawn()
            .unwrap();
        Self { master, child }
    }

    fn write(&self, data: &[u8]) {
        unsafe {
            libc::write(self.master, data.as_ptr().cast(), data.len());
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(self.master, libc::TIOCSWINSZ, &ws);
        }
    }

    fn read_until(&mut self, needle: &str, timeout: Duration) -> String {
        let mut out = Vec::new();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let mut fd = libc::pollfd {
                fd: self.master,
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut fd, 1, 100) };
            if n <= 0 {
                continue;
            }
            let mut buf = [0u8; 4096];
            let len = unsafe { libc::read(self.master, buf.as_mut_ptr().cast(), buf.len()) };
            if len > 0 {
                out.extend_from_slice(&buf[..len as usize]);
                if String::from_utf8_lossy(&out).contains(needle) {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn close_master(&mut self) {
        if self.master >= 0 {
            unsafe {
                libc::close(self.master);
            }
            self.master = -1;
        }
    }

    fn wait(&mut self, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status.code().unwrap_or(-1));
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        None
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.master >= 0 {
            unsafe {
                libc::close(self.master);
            }
        }
    }
}

fn open_pty() -> (i32, i32) {
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0);
        assert_eq!(libc::grantpt(master), 0);
        assert_eq!(libc::unlockpt(master), 0);
        let mut name = [0 as libc::c_char; 128];
        assert_eq!(libc::ptsname_r(master, name.as_mut_ptr(), name.len()), 0);
        let slave = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        assert!(slave >= 0);
        for fd in [master, slave] {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            assert!(flags >= 0);
            assert_eq!(libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC), 0);
        }
        (master, slave)
    }
}

fn set_winsize(fd: i32, rows: u16, cols: u16) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
    }
}

#[test]
fn relays_input_and_output() {
    let mut h = Harness::spawn(&["/bin/sh", "-c", "stty -echo; printf READY; exec cat"]);
    let ready = h.read_until("READY", Duration::from_secs(3));
    assert!(ready.contains("READY"), "out={ready:?}");
    h.write(b"ping\n");
    let out = h.read_until("ping", Duration::from_secs(3));
    assert!(out.contains("ping"), "out={out:?}");
}

#[test]
fn ctrl_c_interrupts_the_host_shell() {
    let mut h = Harness::spawn(&[
        "/bin/sh",
        "-c",
        "stty -echo; printf READY; trap 'printf GOTINT; exit 0' INT; while :; do sleep 0.1; done",
    ]);
    let ready = h.read_until("READY", Duration::from_secs(3));
    assert!(ready.contains("READY"), "out={ready:?}");
    h.write(&[0x03]);
    let out = h.read_until("GOTINT", Duration::from_secs(3));
    assert!(out.contains("GOTINT"), "out={out:?}");
    assert_eq!(h.wait(Duration::from_secs(3)), Some(0));
}

#[test]
fn forwards_outer_resize_to_the_inner_pty() {
    let file = std::env::temp_dir().join(format!("termust-resize-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let script = format!(
        "stty -echo; trap 'stty size > {}; exit 0' WINCH; printf READY; while :; do sleep 0.1; done",
        file.display()
    );
    let mut h = Harness::spawn(&["/bin/sh", "-c", &script]);
    let ready = h.read_until("READY", Duration::from_secs(3));
    assert!(ready.contains("READY"), "out={ready:?}");
    h.resize(30, 100);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut size = String::new();
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&file) {
            size = text;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_file(&file);
    assert_eq!(size.trim(), "30 100", "size={size:?}");
}

#[test]
fn closing_the_outer_pty_stops_the_host_shell() {
    let mut h = Harness::spawn(&["/bin/sh", "-c", "stty -echo; printf READY; sleep 30"]);
    let ready = h.read_until("READY", Duration::from_secs(3));
    assert!(ready.contains("READY"), "out={ready:?}");
    h.close_master();
    assert!(
        h.wait(Duration::from_secs(3)).is_some(),
        "el helper siguió vivo tras cerrar el PTY de transporte"
    );
}

#[test]
fn propagates_the_child_exit_code() {
    let mut h = Harness::spawn(&["/bin/sh", "-c", "exit 7"]);
    assert_eq!(h.wait(Duration::from_secs(3)), Some(7));
}
