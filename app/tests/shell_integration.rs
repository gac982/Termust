use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use termust_core::pty::{PtyEvent, PtySpawner, SpawnSpec};
use termust_core::terminal::Size;
use termust_pty_unix::UnixPtySpawner;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("termust-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_shell(
    shell: &str,
    extra_env: Vec<(OsString, OsString)>,
    home: &Path,
    shim: &Path,
) -> String {
    let si = termust_host_exec::materialize_in(shim).unwrap();
    let cmd = termust_host_exec::shell_command(Path::new(shell), &si, false, None, None);
    let mut env = cmd.env;
    env.push((OsString::from("HOME"), home.as_os_str().to_owned()));
    env.extend(extra_env);

    let spawner = UnixPtySpawner::new().unwrap();
    let mut handle = spawner
        .spawn(SpawnSpec {
            argv: cmd.argv,
            cwd: Some(home.to_path_buf()),
            env,
            size: Size::new(80, 24),
        })
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(6);
    let mut out = Vec::new();
    let mut last_write = Instant::now() - Duration::from_secs(1);
    while Instant::now() < deadline {
        if last_write.elapsed() >= Duration::from_millis(500) {
            handle
                .session
                .write(b"echo TERMUST_IT_$((40+2))\n")
                .unwrap();
            last_write = Instant::now();
        }
        match handle.events.recv_timeout(Duration::from_millis(100)) {
            Ok(PtyEvent::Output(b)) => {
                out.extend_from_slice(&b);
                if String::from_utf8_lossy(&out).contains("TERMUST_IT_42") {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    let _ = handle.session.kill();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn bash_emits_osc7_and_osc133() {
    if !Path::new("/usr/bin/bash").exists() {
        return;
    }
    let root = temp_dir("bash");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let out = run_shell("/usr/bin/bash", Vec::new(), &home, &root.join("si"));
    assert!(out.contains("TERMUST_IT_42"), "output: {out:?}");
    assert!(out.contains("\x1b]7;file://"), "missing OSC 7: {out:?}");
    assert!(out.contains("\x1b]133;"), "missing OSC 133: {out:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zsh_emits_osc7_and_osc133_without_recursion() {
    if !Path::new("/usr/bin/zsh").exists() {
        return;
    }
    let root = temp_dir("zsh");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let out = run_shell(
        "/usr/bin/zsh",
        vec![(
            OsString::from("TERMUST_ORIGINAL_ZDOTDIR"),
            home.as_os_str().to_owned(),
        )],
        &home,
        &root.join("si"),
    );
    assert!(out.contains("TERMUST_IT_42"), "output: {out:?}");
    assert!(out.contains("\x1b]7;file://"), "missing OSC 7: {out:?}");
    assert!(out.contains("\x1b]133;"), "missing OSC 133: {out:?}");
    assert!(
        !out.contains("recursion limit"),
        "zsh shim recursed: {out:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
