//! Integración de remotes y push: keyring, vault y git del host.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;
use termust_git_remote::{
    map_push_error, parse_remotes, push_args, push_env, remote_add_args, remote_remove_args,
    PushFailure, PushRequest, Remote, Transport,
};
use termust_vault::{MasterKey, Secret, Vault, VaultError};

use crate::i18n;

pub fn vault_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("TERMUST_VAULT_PATH") {
        return Ok(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or_else(|| i18n::tr("HOME is not set"))?;
    Ok(base.join("termust/vault.sqlite"))
}

/// Clave maestra: `TERMUST_VAULT_KEY` (tests/desarrollo), o el Secret Service
/// del host vía `secret-tool` (se crea la clave la primera vez).
pub fn master_key() -> Result<MasterKey, String> {
    if let Ok(hex) = std::env::var("TERMUST_VAULT_KEY") {
        return MasterKey::from_hex(&hex).map_err(|error| error.to_string());
    }
    let lookup = secret_tool(
        &["lookup", "service", "termust", "account", "vault-master"],
        None,
    )?;
    let value = String::from_utf8_lossy(&lookup.stdout).trim().to_string();
    if value.len() == 64 {
        return MasterKey::from_hex(&value).map_err(|error| error.to_string());
    }
    let key = MasterKey::generate().map_err(|error| error.to_string())?;
    secret_tool(
        &[
            "store",
            "--label=Termust vault master",
            "service",
            "termust",
            "account",
            "vault-master",
        ],
        Some(key.to_hex().as_bytes()),
    )?;
    Ok(key)
}

fn secret_tool(args: &[&str], stdin: Option<&[u8]>) -> Result<Output, String> {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut command = termust_host_exec::host_command("secret-tool", &args, None);
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        i18n::tr_args(
            "Could not access the keyring: {error}",
            &[("error", &error.to_string())],
        )
    })?;
    if let Some(bytes) = stdin {
        if let Some(mut input) = child.stdin.take() {
            input.write_all(bytes).map_err(|error| {
                i18n::tr_args(
                    "Could not save the key: {error}",
                    &[("error", &error.to_string())],
                )
            })?;
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| i18n::tr_args("keyring: {error}", &[("error", &error.to_string())]))?;
    Ok(output)
}

pub fn open_vault() -> Result<Vault, String> {
    let key = master_key()?;
    Vault::open(&vault_path()?, key).map_err(|error| error.to_string())
}

pub fn load_credentials(origin: &str) -> Result<Option<(String, Secret)>, String> {
    let vault = open_vault()?;
    match vault.get(origin) {
        Ok(credential) => Ok(Some((credential.username, credential.secret))),
        Err(VaultError::Missing) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub fn save_credentials(origin: &str, username: &str, secret: &[u8]) -> Result<(), String> {
    let mut vault = open_vault()?;
    vault
        .put(origin, username, secret)
        .map_err(|error| error.to_string())
}

pub fn list_remotes(cwd: &Path) -> Result<Vec<Remote>, String> {
    let output = run_git(cwd, &["remote", "-v"])?;
    Ok(parse_remotes(&String::from_utf8_lossy(&output.stdout)))
}

pub fn add_remote(cwd: &Path, name: &str, url: &str) -> Result<(), String> {
    let args = remote_add_args(name, url).map_err(|error| error.to_string())?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_git(cwd, &args)?;
    Ok(())
}

pub fn remove_remote(cwd: &Path, name: &str) -> Result<(), String> {
    let args = remote_remove_args(name).map_err(|error| error.to_string())?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_git(cwd, &args)?;
    Ok(())
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<Output, String> {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let output = termust_host_exec::host_command("git", &args, Some(cwd))
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| {
            i18n::tr_args(
                "could not run git: {error}",
                &[("error", &error.to_string())],
            )
        })?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(output)
}

/// Push de una rama. Devuelve un mensaje de éxito o un error accionable.
pub fn push(
    cwd: &Path,
    remote: &str,
    refspec: &str,
    transport: Transport,
    credentials: Option<(String, Secret)>,
) -> Result<String, String> {
    let request = PushRequest {
        remote: remote.to_string(),
        refspec: refspec.to_string(),
        set_upstream: true,
    };
    let args = push_args(&request, transport).map_err(|error| error.to_string())?;
    let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();

    if transport == Transport::Ssh {
        let env = push_env(Transport::Ssh, None, None, None);
        let output = termust_host_exec::host_command_with_env("git", &args, Some(cwd), &env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| {
                i18n::tr_args(
                    "could not run git: {error}",
                    &[("error", &error.to_string())],
                )
            })?;
        return finish_push(output);
    }

    let (script, user_fifo, pass_fifo) = askpass_files()?;
    let env = push_env(
        Transport::Https,
        Some(&script),
        Some(&user_fifo),
        Some(&pass_fifo),
    );
    let mut child = termust_host_exec::host_command_with_env("git", &args, Some(cwd), &env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            i18n::tr_args(
                "could not run git: {error}",
                &[("error", &error.to_string())],
            )
        })?;
    let Some((username, secret)) = credentials else {
        child.kill().ok();
        cleanup_askpass(&user_fifo, &pass_fifo);
        return Err(i18n::tr("Missing HTTPS credentials for this origin"));
    };
    let password = secret
        .as_str()
        .map_err(|error| error.to_string())?
        .to_string();
    let user_writer = user_fifo.clone();
    let pass_writer = pass_fifo.clone();
    let writer = std::thread::spawn(move || {
        write_fifo(&user_writer, username.as_bytes())?;
        write_fifo(&pass_writer, password.as_bytes())
    });
    let output = child
        .wait_with_output()
        .map_err(|error| i18n::tr_args("git push: {error}", &[("error", &error.to_string())]))?;
    let fifo_result = writer.join().unwrap_or(Err(i18n::tr("askpass failed")));
    cleanup_askpass(&user_fifo, &pass_fifo);
    fifo_result?;
    finish_push(output)
}

fn finish_push(output: Output) -> Result<String, String> {
    if output.status.success() {
        return Ok("Push completado".into());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let failure = map_push_error(&stderr);
    if failure == PushFailure::Other(String::new()) {
        return Err(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    Err(failure.message())
}

fn state_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or_else(|| i18n::tr("HOME is not set"))?;
    let dir = base.join("termust/git");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    Ok(dir)
}

const ASKPASS: &str = r#"#!/bin/sh
case "$1" in
  *sername*) cat "$TERMUST_ASKPASS_USER_FIFO" ;;
  *) cat "$TERMUST_ASKPASS_PASS_FIFO" ;;
esac
"#;

fn askpass_files() -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let dir = state_dir()?;
    let script = dir.join("askpass.sh");
    std::fs::write(&script, ASKPASS).map_err(|error| error.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let user = dir.join(format!("user-{id}.fifo"));
    let pass = dir.join(format!("pass-{id}.fifo"));
    make_fifo(&user)?;
    make_fifo(&pass)?;
    Ok((script, user, pass))
}

fn make_fifo(path: &Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| i18n::tr("invalid FIFO path"))?;
    // SAFETY: c_path es un CString válido y vive durante la llamada.
    let result = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    if result != 0 {
        return Err(i18n::tr_args(
            "could not create the FIFO: {error}",
            &[("error", &std::io::Error::last_os_error().to_string())],
        ));
    }
    Ok(())
}

fn write_fifo(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| i18n::tr("invalid FIFO path"))?;
    let mut fd = -1;
    for _ in 0..500 {
        // SAFETY: c_path es válido; O_NONBLOCK evita bloquear si git aún no
        // abrió el FIFO para lectura.
        fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd >= 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if fd < 0 {
        return Err(i18n::tr("git did not request credentials in time"));
    }
    let mut written = 0;
    while written < data.len() {
        // SAFETY: fd es válido y data/written están dentro del buffer.
        let result =
            unsafe { libc::write(fd, data[written..].as_ptr().cast(), data.len() - written) };
        if result <= 0 {
            // SAFETY: fd se cierra abajo; un fallo de escritura termina el loop.
            unsafe { libc::close(fd) };
            return Err(i18n::tr("could not write the credential to the FIFO"));
        }
        written += result as usize;
    }
    // SAFETY: fd fue abierto arriba y no se usa después.
    unsafe { libc::close(fd) };
    Ok(())
}

fn cleanup_askpass(user: &Path, pass: &Path) {
    let _ = std::fs::remove_file(user);
    let _ = std::fs::remove_file(pass);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn askpass_fifos_deliver_without_secret_in_argv() {
        let (script, user_fifo, pass_fifo) = askpass_files().expect("askpass");
        let user_writer = user_fifo.clone();
        let pass_writer = pass_fifo.clone();
        let writer = std::thread::spawn(move || {
            write_fifo(&user_writer, b"alice").unwrap();
            write_fifo(&pass_writer, b"TOKEN-SECRETO").unwrap();
        });
        let user_fifo_2 = user_fifo.clone();
        let pass_fifo_2 = pass_fifo.clone();
        let script_2 = script.clone();
        let password = std::thread::spawn(move || {
            std::process::Command::new("sh")
                .arg(&script_2)
                .arg("Password for 'https://example.com': ")
                .env("TERMUST_ASKPASS_USER_FIFO", &user_fifo_2)
                .env("TERMUST_ASKPASS_PASS_FIFO", &pass_fifo_2)
                .output()
                .expect("askpass password")
        });
        let output = std::process::Command::new("sh")
            .arg(&script)
            .arg("Username for 'https://example.com': ")
            .env("TERMUST_ASKPASS_USER_FIFO", &user_fifo)
            .env("TERMUST_ASKPASS_PASS_FIFO", &pass_fifo)
            .output()
            .expect("askpass user");
        let password = password.join().unwrap();
        writer.join().unwrap();
        assert!(output.status.success());
        assert!(password.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "alice");
        assert_eq!(
            String::from_utf8_lossy(&password.stdout).trim(),
            "TOKEN-SECRETO"
        );
        // El secreto llega a git por stdout del helper, nunca por argv/env.
        assert!(!script.to_string_lossy().contains("TOKEN-SECRETO"));
        cleanup_askpass(&user_fifo, &pass_fifo);
        let _ = std::fs::remove_file(script);
    }
}
