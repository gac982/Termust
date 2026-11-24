//! Backup local de configuración, sesión y datos de la app.
//!
//! Se guarda fuera del sandbox (`~/.local/share/termust/backups`) para
//! sobrevivir a cambios de app ID, reinstalaciones y `--delete-data`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const KEEP: usize = 5;

pub fn backup_state(home: &Path) {
    let keyfile = gtk4::glib::user_config_dir()
        .join("glib-2.0")
        .join("settings")
        .join("keyfile");
    let session = termust_store::session_path();
    let vault = gtk4::glib::user_data_dir()
        .join("termust")
        .join("vault.sqlite");
    let themes = home.join(".config").join("termust").join("themes");

    if !keyfile.exists() && !session.exists() && !vault.exists() && !themes.exists() {
        return;
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let root = home
        .join(".local")
        .join("share")
        .join("termust")
        .join("backups");
    let dest = root.join(format!("{}-{stamp}", env!("CARGO_PKG_VERSION")));
    if fs::create_dir_all(&dest).is_err() {
        return;
    }

    if keyfile.exists() {
        let _ = fs::copy(&keyfile, dest.join("settings.keyfile"));
    }
    if session.exists() {
        let _ = fs::copy(&session, dest.join("session.json"));
    }
    if vault.exists() {
        let _ = fs::copy(&vault, dest.join("vault.sqlite"));
    }
    if themes.is_dir() {
        copy_dir(&themes, &dest.join("themes"));
    }

    prune(&root);
}

fn copy_dir(from: &Path, to: &Path) {
    let Ok(entries) = fs::read_dir(from) else {
        return;
    };
    let _ = fs::create_dir_all(to);
    for entry in entries.flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            let _ = fs::copy(&path, &target);
        }
    }
}

fn prune(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    while dirs.len() > KEEP {
        let oldest = dirs.remove(0);
        let _ = fs::remove_dir_all(oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_and_prune() {
        let root = std::env::temp_dir().join(format!(
            "termust-backup-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let src = root.join("src");
        fs::create_dir_all(src.join("themes")).unwrap();
        fs::write(src.join("themes/theme.toml"), "x").unwrap();
        let dst = root.join("dst");
        copy_dir(&src, &dst);
        assert!(dst.join("themes/theme.toml").exists());

        let backups = root.join("backups");
        for index in 0..(KEEP + 2) {
            fs::create_dir_all(backups.join(format!("v-{index:02}"))).unwrap();
        }
        prune(&backups);
        assert_eq!(fs::read_dir(&backups).unwrap().count(), KEEP);

        let _ = fs::remove_dir_all(root);
    }
}
