//! Ejecución en el host y materialización de la integración de shells.
//!
//! Es el único crate que puede construir `flatpak-spawn` (§5.3).

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use termust_core::shell::resolve_shell;

const BASH_RC: &str = include_str!("../../../data/shell-integration/bash/rc.bash");
const ZSH_ENV: &str = include_str!("../../../data/shell-integration/zsh/.zshenv");
const ZSH_PROFILE: &str = include_str!("../../../data/shell-integration/zsh/.zprofile");
const ZSH_RC: &str = include_str!("../../../data/shell-integration/zsh/.zshrc");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    NoHome,
    Io(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::NoHome => write!(f, "HOME is not set"),
            HostError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for HostError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegration {
    pub bash_rc: PathBuf,
    pub zsh_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    pub argv: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

/// Bundle del helper que aloja el PTY real en el host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPty {
    pub loader: PathBuf,
    pub lib_dir: PathBuf,
    pub helper: PathBuf,
}

pub fn is_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

pub fn default_shell() -> PathBuf {
    if is_flatpak() {
        if let Some(shell) = host_login_shell() {
            return shell;
        }
    }
    resolve_shell(
        None,
        std::env::var_os("SHELL").as_ref(),
        Some(&PathBuf::from("/bin/sh")),
    )
}

/// Lee el shell de login del usuario en el host (no el $SHELL del sandbox).
fn host_login_shell() -> Option<PathBuf> {
    let uid = unsafe { libc::getuid() }.to_string();
    let out = std::process::Command::new("flatpak-spawn")
        .args(["--host", "getent", "passwd", &uid])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let shell = line.trim().rsplit(':').next()?;
    if shell.starts_with('/') {
        Some(PathBuf::from(shell))
    } else {
        None
    }
}

fn state_base() -> Result<PathBuf, HostError> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .ok_or(HostError::NoHome)
}

pub fn materialize() -> Result<ShellIntegration, HostError> {
    let base = state_base()?;
    materialize_in(&base.join("termust/shell-integration"))
}

static HOST_PTY: OnceLock<Option<HostPty>> = OnceLock::new();

/// Bundle del PTY del host, materializado una sola vez por proceso.
pub fn host_pty() -> Option<HostPty> {
    HOST_PTY
        .get_or_init(|| materialize_host_pty().ok().flatten())
        .clone()
}

/// Copia el helper y las librerías del runtime a una ruta visible desde el
/// host. Dentro del Flatpak `$HOME` es el home real (el manifiesto pide
/// `--filesystem=host`), así que la misma ruta vale en ambos lados.
fn materialize_host_pty() -> Result<Option<HostPty>, HostError> {
    if !is_flatpak() {
        return Ok(None);
    }
    let source = Path::new("/app/libexec/termust-host-pty");
    if !source.exists() {
        return Ok(None);
    }
    let bytes = fs::read(source).map_err(|e| HostError::Io(e.to_string()))?;
    let hash = fnv1a(&bytes);
    let base = state_base()?;
    let dir = base.join("termust/host-pty").join(format!("{hash:016x}"));
    let marker = dir.join(".ready");

    if !marker.exists() {
        let tmp = dir.with_file_name(format!(".tmp-{}-{}", std::process::id(), hash));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| HostError::Io(e.to_string()))?;
        copy_exec(source, &tmp.join("termust-host-pty"))?;

        let deps = helper_deps(source);
        let mut loader_source = None;
        for dep in deps {
            let Some(name) = dep.file_name() else {
                continue;
            };
            if name.to_string_lossy().starts_with("ld-linux") {
                loader_source = Some(dep);
                continue;
            }
            let _ = fs::copy(&dep, tmp.join(name));
        }
        let loader_source = loader_source
            .or_else(default_loader)
            .ok_or_else(|| HostError::Io("no se encontró ld-linux en el runtime".into()))?;
        copy_exec(&loader_source, &tmp.join("ld-linux-x86-64.so.2"))?;
        fs::write(tmp.join(".ready"), b"ok").map_err(|e| HostError::Io(e.to_string()))?;

        if let Err(e) = fs::rename(&tmp, &dir) {
            let _ = fs::remove_dir_all(&tmp);
            if !dir.join(".ready").exists() {
                return Err(HostError::Io(e.to_string()));
            }
        }
    }

    let bundle = HostPty {
        loader: dir.join("ld-linux-x86-64.so.2"),
        lib_dir: dir.clone(),
        helper: dir.join("termust-host-pty"),
    };
    if !bundle.loader.exists() || !bundle.helper.exists() {
        return Ok(None);
    }
    if !host_bundle_runs(&bundle) {
        eprintln!(
            "termust: no se pudo ejecutar el helper del PTY del host;              Ctrl+C/Ctrl+Z no funcionarán en el shell del host"
        );
        return Ok(None);
    }
    Ok(Some(bundle))
}

fn copy_exec(from: &Path, to: &Path) -> Result<(), HostError> {
    fs::copy(from, to).map_err(|e| HostError::Io(e.to_string()))?;
    fs::set_permissions(to, fs::Permissions::from_mode(0o755))
        .map_err(|e| HostError::Io(e.to_string()))?;
    Ok(())
}

fn helper_deps(source: &Path) -> Vec<PathBuf> {
    let Ok(out) = Command::new("ldd").arg(source).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut seen = HashSet::new();
    text.lines()
        .filter_map(parse_ldd_line)
        .filter(|path| path.exists() && seen.insert(path.clone()))
        .collect()
}

/// Extrae la ruta absoluta de una línea de `ldd`.
fn parse_ldd_line(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim();
    if trimmed.starts_with("linux-vdso") || trimmed.starts_with("statically linked") {
        return None;
    }
    let path = if let Some((_, rest)) = trimmed.split_once("=>") {
        rest.split_whitespace().next()?
    } else {
        trimmed.split_whitespace().next()?
    };
    if path.starts_with('/') {
        Some(PathBuf::from(path))
    } else {
        None
    }
}

fn default_loader() -> Option<PathBuf> {
    [
        "/lib64/ld-linux-x86-64.so.2",
        "/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
        "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

/// Comprueba que el loader y las libs copiadas funcionan en el host.
fn host_bundle_runs(bundle: &HostPty) -> bool {
    let Ok(out) = Command::new("flatpak-spawn")
        .arg("--host")
        .arg(&bundle.loader)
        .arg("--library-path")
        .arg(&bundle.lib_dir)
        .arg(&bundle.helper)
        .output()
    else {
        return false;
    };
    // Sin argumentos el helper imprime el uso y sale con 2.
    out.status.code() == Some(2)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn materialize_in(root: &Path) -> Result<ShellIntegration, HostError> {
    let hash = content_hash();
    let dir = root.join(hash);
    let bash_rc = dir.join("bash.bashrc");
    let zsh_dir = dir.join("zsh");

    if bash_rc.exists() && zsh_dir.join(".zshrc").exists() {
        return Ok(ShellIntegration { bash_rc, zsh_dir });
    }

    fs::create_dir_all(&zsh_dir).map_err(|e| HostError::Io(e.to_string()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| HostError::Io(e.to_string()))?;
    fs::set_permissions(&zsh_dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| HostError::Io(e.to_string()))?;

    write_atomic(&bash_rc, BASH_RC)?;
    write_atomic(&zsh_dir.join(".zshenv"), ZSH_ENV)?;
    write_atomic(&zsh_dir.join(".zprofile"), ZSH_PROFILE)?;
    write_atomic(&zsh_dir.join(".zshrc"), ZSH_RC)?;

    Ok(ShellIntegration { bash_rc, zsh_dir })
}

pub fn shell_command(
    shell: &Path,
    integration: &ShellIntegration,
    flatpak: bool,
    cwd: Option<&Path>,
    host_pty: Option<&HostPty>,
) -> ShellCommand {
    let name = shell
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("sh")
        .to_string();

    let mut env = vec![
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("TERM_PROGRAM".into(), "Termust".into()),
        (
            "TERM_PROGRAM_VERSION".into(),
            env!("CARGO_PKG_VERSION").into(),
        ),
    ];

    let mut inner: Vec<OsString> = Vec::new();
    if name == "zsh" {
        inner.push(shell.as_os_str().to_owned());
        inner.push("-i".into());
        if let Some(home) = std::env::var_os("HOME") {
            env.push(("TERMUST_ORIGINAL_ZDOTDIR".into(), home));
        }
        env.push(("ZDOTDIR".into(), integration.zsh_dir.as_os_str().to_owned()));
    } else if name == "bash" {
        inner.push(shell.as_os_str().to_owned());
        inner.push("--rcfile".into());
        inner.push(integration.bash_rc.as_os_str().to_owned());
        inner.push("-i".into());
    } else {
        inner.push(shell.as_os_str().to_owned());
        inner.push("-i".into());
    }

    if !flatpak {
        return ShellCommand { argv: inner, env };
    }

    let mut argv: Vec<OsString> = vec![
        "flatpak-spawn".into(),
        "--host".into(),
        "--watch-bus".into(),
    ];
    for (k, v) in &env {
        let mut pair = OsString::from("--env=");
        pair.push(k);
        pair.push("=");
        pair.push(v);
        argv.push(pair);
    }
    if let Some(dir) = cwd {
        let mut pair = OsString::from("--directory=");
        pair.push(dir);
        argv.push(pair);
    }
    argv.push("--".into());
    if let Some(bundle) = host_pty {
        argv.push(bundle.loader.as_os_str().to_owned());
        argv.push("--library-path".into());
        argv.push(bundle.lib_dir.as_os_str().to_owned());
        argv.push(bundle.helper.as_os_str().to_owned());
    }
    argv.extend(inner);
    ShellCommand {
        argv,
        env: Vec::new(),
    }
}

fn write_atomic(path: &Path, content: &str) -> Result<(), HostError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content).map_err(|e| HostError::Io(e.to_string()))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))
        .map_err(|e| HostError::Io(e.to_string()))?;
    fs::rename(&tmp, path).map_err(|e| HostError::Io(e.to_string()))?;
    Ok(())
}

fn content_hash() -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for part in ["v1", BASH_RC, ZSH_ENV, ZSH_PROFILE, ZSH_RC] {
        for b in part.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_contain_osc7_and_osc133() {
        assert!(BASH_RC.contains("]7;"));
        assert!(BASH_RC.contains("]133;"));
        assert!(ZSH_RC.contains("]7;"));
        assert!(ZSH_RC.contains("]133;"));
        assert!(ZSH_ENV.contains("TERMUST_ORIGINAL_ZDOTDIR"));
        assert!(ZSH_RC.contains("export ZDOTDIR"));
    }

    #[test]
    fn materialize_writes_private_files() {
        let root = std::env::temp_dir().join(format!("termust-si-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let si = materialize_in(&root).unwrap();
        assert!(si.bash_rc.exists());
        assert!(si.zsh_dir.join(".zshenv").exists());
        let mode = fs::metadata(&si.bash_rc).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        let dir_mode = fs::metadata(si.bash_rc.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn direct_bash_argv_has_rcfile() {
        let si = ShellIntegration {
            bash_rc: PathBuf::from("/tmp/rc"),
            zsh_dir: PathBuf::from("/tmp/zsh"),
        };
        let cmd = shell_command(Path::new("/usr/bin/bash"), &si, false, None, None);
        assert_eq!(cmd.argv[0], OsString::from("/usr/bin/bash"));
        assert!(cmd.argv.iter().any(|a| a == "--rcfile"));
        assert!(cmd
            .env
            .iter()
            .any(|(k, v)| k == "TERM_PROGRAM" && v == "Termust"));
    }

    #[test]
    fn flatpak_zsh_argv_uses_zdotdir() {
        let si = ShellIntegration {
            bash_rc: PathBuf::from("/tmp/rc"),
            zsh_dir: PathBuf::from("/tmp/zsh"),
        };
        let cmd = shell_command(Path::new("/usr/bin/zsh"), &si, true, None, None);
        assert_eq!(cmd.argv[0], OsString::from("flatpak-spawn"));
        assert!(cmd.argv.iter().any(|a| a == "--host"));
        assert!(cmd
            .argv
            .iter()
            .any(|a| a.to_string_lossy().starts_with("--env=ZDOTDIR=/tmp/zsh")));
        assert!(!cmd.argv.iter().any(|a| a == "--env"));
        assert!(cmd.env.is_empty());
    }

    #[test]
    fn ldd_lines_are_parsed() {
        assert_eq!(
            parse_ldd_line("\tlibc.so.6 => /usr/lib/x86_64-linux-gnu/libc.so.6 (0x00007f)"),
            Some(PathBuf::from("/usr/lib/x86_64-linux-gnu/libc.so.6"))
        );
        assert_eq!(
            parse_ldd_line("/lib64/ld-linux-x86-64.so.2 (0x00007f)"),
            Some(PathBuf::from("/lib64/ld-linux-x86-64.so.2"))
        );
        assert_eq!(parse_ldd_line("linux-vdso.so.1 (0x00007f)"), None);
        assert_eq!(parse_ldd_line("statically linked"), None);
    }

    fn host_pty() -> HostPty {
        HostPty {
            loader: PathBuf::from("/home/u/.local/state/termust/host-pty/x/ld-linux-x86-64.so.2"),
            lib_dir: PathBuf::from("/home/u/.local/state/termust/host-pty/x"),
            helper: PathBuf::from("/home/u/.local/state/termust/host-pty/x/termust-host-pty"),
        }
    }

    #[test]
    fn flatpak_argv_uses_host_pty_and_directory() {
        let si = ShellIntegration {
            bash_rc: PathBuf::from("/tmp/rc"),
            zsh_dir: PathBuf::from("/tmp/zsh"),
        };
        let hp = host_pty();
        let cmd = shell_command(
            Path::new("/usr/bin/zsh"),
            &si,
            true,
            Some(Path::new("/home/u/repo")),
            Some(&hp),
        );
        let argv: Vec<String> = cmd
            .argv
            .iter()
            .map(|a| a.to_string_lossy().into())
            .collect();
        assert!(argv.iter().any(|a| a == "--directory=/home/u/repo"));
        let sep = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(argv[sep + 1], hp.loader.to_string_lossy());
        assert_eq!(argv[sep + 2], "--library-path");
        assert_eq!(argv[sep + 3], hp.lib_dir.to_string_lossy());
        assert_eq!(argv[sep + 4], hp.helper.to_string_lossy());
        assert_eq!(argv[sep + 5], "/usr/bin/zsh");
        assert_eq!(argv[sep + 6], "-i");
        assert!(cmd.env.is_empty());
    }

    #[test]
    fn unknown_shell_runs_without_integration() {
        let si = ShellIntegration {
            bash_rc: PathBuf::from("/tmp/rc"),
            zsh_dir: PathBuf::from("/tmp/zsh"),
        };
        let cmd = shell_command(Path::new("/usr/bin/fish"), &si, false, None, None);
        assert_eq!(
            cmd.argv,
            vec![OsString::from("/usr/bin/fish"), OsString::from("-i")]
        );
    }
}
