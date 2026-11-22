//! Aloja el PTY del shell en el host cuando Termust corre como Flatpak.
//!
//! El PTY del sandbox queda solo como transporte: este helper pone el terminal
//! de entrada en modo raw (para que `Ctrl+C` llegue como byte en vez de generar
//! una señal en el sandbox), crea el PTY real en el host, pone al shell como
//! líder de sesión de ese PTY y retransmite bytes en ambos sentidos. Además
//! copia el tamaño cada 150 ms, porque el `SIGWINCH` del PTY de transporte no
//! llega al host.
//!
//! Se invoca como: `termust-host-pty <shell> [args...]`.

use std::ffi::CString;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;

/// Restaura los atributos del terminal de transporte al salir.
struct TermiosGuard {
    fd: i32,
    original: libc::termios,
    active: bool,
}

impl TermiosGuard {
    fn set_raw(fd: i32) -> Option<Self> {
        unsafe {
            let mut original: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut original) != 0 {
                return None;
            }
            let mut raw = original;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return None;
            }
            Some(Self {
                fd,
                original,
                active: true,
            })
        }
    }

    fn restore(&mut self) {
        if self.active {
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
            }
            self.active = false;
        }
    }
}

impl Drop for TermiosGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

fn debug(msg: &str) {
    let Some(path) = std::env::var_os("TERMUST_HOST_PTY_LOG") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{msg}");
    }
}

fn main() {
    debug("start");
    let argv: Vec<CString> = std::env::args_os()
        .skip(1)
        .filter(|a| a != "--")
        .filter_map(|a| CString::new(a.as_bytes()).ok())
        .collect();
    if argv.is_empty() {
        eprintln!("termust-host-pty: falta el comando a ejecutar");
        std::process::exit(2);
    }

    let mut termios = TermiosGuard::set_raw(0);

    let (master, slave) = match open_pty() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("termust-host-pty: no se pudo abrir el PTY: {e}");
            std::process::exit(1);
        }
    };

    if let Some(ws) = get_winsize(0) {
        set_winsize(master, &ws);
    }

    let mut ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    ptrs.push(std::ptr::null());

    debug(&format!("outer_ws={:?}", get_winsize(0)));
    let pid = unsafe { libc::fork() };
    debug(&format!("fork pid={pid}"));
    if pid < 0 {
        eprintln!("termust-host-pty: fork: {}", io::Error::last_os_error());
        std::process::exit(1);
    }

    if pid == 0 {
        unsafe {
            libc::close(master);
            if libc::setsid() < 0 {
                child_fail("setsid");
            }
            if libc::ioctl(slave, libc::TIOCSCTTY, 0) < 0 {
                child_fail("TIOCSCTTY");
            }
            libc::dup2(slave, 0);
            libc::dup2(slave, 1);
            libc::dup2(slave, 2);
            if slave > 2 {
                libc::close(slave);
            }
            libc::execvp(argv[0].as_ptr(), ptrs.as_ptr());
            child_fail("execvp");
        }
    }

    unsafe {
        libc::close(slave);
    }

    let code = relay(pid, master);
    if let Some(t) = termios.as_mut() {
        t.restore();
    }
    std::process::exit(code);
}

unsafe fn child_fail(what: &str) -> ! {
    let err = io::Error::last_os_error();
    let msg = format!("termust-host-pty: {what}: {err}\n");
    libc::write(2, msg.as_ptr().cast(), msg.len());
    libc::_exit(127);
}

fn open_pty() -> io::Result<(i32, i32)> {
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        if master < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
            let err = io::Error::last_os_error();
            libc::close(master);
            return Err(err);
        }
        let mut name = [0 as libc::c_char; 128];
        if libc::ptsname_r(master, name.as_mut_ptr(), name.len()) != 0 {
            let err = io::Error::last_os_error();
            libc::close(master);
            return Err(err);
        }
        let slave = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        if slave < 0 {
            let err = io::Error::last_os_error();
            libc::close(master);
            return Err(err);
        }
        set_cloexec(master);
        set_cloexec(slave);
        Ok((master, slave))
    }
}

fn set_cloexec(fd: i32) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

fn get_winsize(fd: i32) -> Option<libc::winsize> {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            Some(ws)
        } else {
            None
        }
    }
}

fn set_winsize(fd: i32, ws: &libc::winsize) {
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, ws);
    }
}

fn relay(pid: libc::pid_t, master: i32) -> i32 {
    let mut buf = [0u8; 65536];
    let mut last_ws = get_winsize(0);
    let mut status: i32 = 0;

    loop {
        if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == pid {
            break;
        }

        let mut fds = [
            libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: master,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, 150) };
        debug(&format!(
            "poll n={n} in={} out={}",
            fds[0].revents, fds[1].revents
        ));
        if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break;
        }

        if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            let len = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            debug(&format!("stdin read len={len}"));
            if len == 0 {
                // El sandbox cerró el PTY de transporte: terminar el shell.
                unsafe {
                    libc::kill(pid, libc::SIGHUP);
                }
                break;
            }
            if len > 0 {
                write_all(master, &buf[..len as usize]);
            } else {
                let err = io::Error::last_os_error();
                match err.kind() {
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => {}
                    // EIO/HUP: el otro extremo del PTY de transporte murió.
                    _ => {
                        unsafe {
                            libc::kill(pid, libc::SIGHUP);
                        }
                        break;
                    }
                }
            }
        }

        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            let len = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
            if len <= 0 {
                // EIO: el shell cerró el PTY.
                break;
            }
            write_all(1, &buf[..len as usize]);
        }

        if let Some(ws) = get_winsize(0) {
            let changed = last_ws
                .as_ref()
                .map(|last| last.ws_row != ws.ws_row || last.ws_col != ws.ws_col)
                .unwrap_or(true);
            if changed {
                set_winsize(master, &ws);
                last_ws = Some(ws);
            }
        }
    }

    if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == 0 {
        unsafe {
            libc::kill(pid, libc::SIGHUP);
            libc::waitpid(pid, &mut status, 0);
        }
    }
    drain_nonblocking(master);
    // Cerrar el master del PTY interno cuelga la sesión: el foreground
    // (btop, sleep, etc.) recibe SIGHUP aunque no sea hijo del shell.
    unsafe {
        libc::close(master);
    }
    exit_code(status)
}

fn drain_nonblocking(master: i32) {
    unsafe {
        let flags = libc::fcntl(master, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let mut buf = [0u8; 8192];
    loop {
        let len = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        if len <= 0 {
            break;
        }
        write_all(1, &buf[..len as usize]);
    }
}

fn write_all(fd: i32, mut data: &[u8]) {
    while !data.is_empty() {
        let len = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if len < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        data = &data[len as usize..];
    }
}

fn exit_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}
