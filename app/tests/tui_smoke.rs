use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};
use termust_core::pty::{PtyEvent, PtyHandle, PtySpawner, SpawnSpec};
use termust_core::terminal::{Size, TerminalEngine};
use termust_pty_unix::UnixPtySpawner;
use termust_term_engine::AlacrittyEngine;

fn spawn(argv: &[&str], home: &Path) -> (PtyHandle, AlacrittyEngine) {
    let spawner = UnixPtySpawner::new().unwrap();
    let handle = spawner
        .spawn(SpawnSpec {
            argv: argv.iter().map(OsString::from).collect(),
            cwd: Some(home.to_path_buf()),
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
                ("HOME".into(), home.as_os_str().to_owned()),
            ],
            size: Size::new(80, 24),
        })
        .unwrap();
    (handle, AlacrittyEngine::new(Size::new(80, 24), 2_000))
}

fn screen_contains(engine: &mut AlacrittyEngine, needle: &str) -> bool {
    let snap = engine.snapshot();
    (0..snap.size.rows).any(|r| snap.row_text(r).contains(needle))
}

#[test]
fn less_renders_file_and_quits() {
    if !Path::new("/usr/bin/less").exists() {
        return;
    }
    let root = std::env::temp_dir().join(format!("termust-less-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("sample.txt");
    std::fs::write(&file, "TERMUST_LESS_MARKER\nsecond line\n").unwrap();

    let (mut handle, mut engine) = spawn(&["/usr/bin/less", file.to_str().unwrap()], &root);
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut found = false;
    while Instant::now() < deadline {
        match handle.events.recv_timeout(Duration::from_millis(100)) {
            Ok(PtyEvent::Output(b)) => {
                engine.feed(&b);
                for r in engine.take_responses() {
                    let _ = handle.session.write(&r);
                }
                if screen_contains(&mut engine, "TERMUST_LESS_MARKER") {
                    found = true;
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    let _ = handle.session.write(b"q");
    let _ = handle.session.kill();
    assert!(found, "less did not render the file");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn vim_inserts_and_saves() {
    if !Path::new("/usr/bin/vim").exists() {
        return;
    }
    let root = std::env::temp_dir().join(format!("termust-vim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("edited.txt");
    std::fs::write(&file, "").unwrap();

    let (mut handle, mut engine) = spawn(
        &[
            "/usr/bin/vim",
            "-u",
            "NONE",
            "-N",
            "-n",
            file.to_str().unwrap(),
        ],
        &root,
    );
    let warmup = Instant::now() + Duration::from_millis(800);
    while Instant::now() < warmup {
        if let Ok(PtyEvent::Output(b)) = handle.events.recv_timeout(Duration::from_millis(100)) {
            engine.feed(&b);
            for r in engine.take_responses() {
                let _ = handle.session.write(&r);
            }
        }
    }
    handle
        .session
        .write(b"ihello from termust\x1b:wq!\r")
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if Instant::now() >= deadline {
            let snap = engine.snapshot();
            let lines: Vec<String> = (0..snap.size.rows).map(|r| snap.row_text(r)).collect();
            let _ = handle.session.kill();
            panic!("vim did not exit; screen={lines:?}");
        }
        match handle.events.recv_timeout(Duration::from_millis(100)) {
            Ok(PtyEvent::Exited(code)) => {
                assert_eq!(code, Some(0));
                break;
            }
            Ok(PtyEvent::Output(b)) => {
                engine.feed(&b);
                for r in engine.take_responses() {
                    let _ = handle.session.write(&r);
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    let content = std::fs::read_to_string(&file).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&root);
    assert!(content.contains("hello from termust"), "file: {content:?}");
}
