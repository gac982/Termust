//! Adaptador de filesystem local: listado ordenado y metadatos de solo lectura.
//!
//! El watch vive en la app (GIO `FileMonitor`), para no agregar dependencias ni
//! hilos: el listado es una función pura y testeable.
//!
//! Reglas:
//! - Solo lectura. No existe API de escritura, borrado, rename ni move.
//! - Los directorios van primero y el orden es case-insensitive.
//! - Los symlinks rotos se listan como `BrokenSymlink` y no son navegables.
//! - Nunca se sigue un symlink para listar; la expansión es explícita.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    SymlinkToDir,
    SymlinkToFile,
    BrokenSymlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    /// Directorio real o symlink que apunta a un directorio.
    pub is_dir: bool,
    /// Se puede abrir como carpeta (permisos de lectura/ejecución).
    pub readable: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsError {
    NotFound(PathBuf),
    NotADirectory(PathBuf),
    PermissionDenied(PathBuf),
    Io(String),
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FsError::NotFound(path) => write!(
                f,
                "{}: {}",
                termust_i18n::tr("Does not exist"),
                path.display()
            ),
            FsError::NotADirectory(path) => write!(
                f,
                "{}: {}",
                termust_i18n::tr("Not a directory"),
                path.display()
            ),
            FsError::PermissionDenied(path) => {
                write!(
                    f,
                    "{}: {}",
                    termust_i18n::tr("No permission to read"),
                    path.display()
                )
            }
            FsError::Io(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for FsError {}

/// Lee un directorio y devuelve las entradas ordenadas (carpetas primero).
pub fn list_dir(path: &Path, include_hidden: bool) -> Result<Vec<FileEntry>, FsError> {
    // Se sigue el symlink final: un cwd lógico puede ser un enlace a carpeta.
    let metadata = std::fs::metadata(path).map_err(|error| map_error(path, &error))?;
    if !metadata.is_dir() {
        return Err(FsError::NotADirectory(path.to_path_buf()));
    }
    let read = std::fs::read_dir(path).map_err(|error| map_error(path, &error))?;
    let mut entries = Vec::new();
    for item in read {
        let Ok(item) = item else {
            continue;
        };
        let name = item.file_name().to_string_lossy().to_string();
        let hidden = name.starts_with('.');
        if hidden && !include_hidden {
            continue;
        }
        let file_type = item.file_type();
        let entry_path = item.path();
        let (kind, is_dir) = match file_type {
            Ok(kind) if kind.is_symlink() => match std::fs::metadata(&entry_path) {
                Ok(target) if target.is_dir() => (EntryKind::SymlinkToDir, true),
                Ok(_) => (EntryKind::SymlinkToFile, false),
                Err(_) => (EntryKind::BrokenSymlink, false),
            },
            Ok(kind) if kind.is_dir() => (EntryKind::Dir, true),
            Ok(kind) if kind.is_file() => (EntryKind::File, false),
            _ => (EntryKind::Other, false),
        };
        let readable = !is_dir || can_open_dir(&entry_path);
        entries.push(FileEntry {
            path: entry_path,
            name,
            kind,
            is_dir,
            readable,
            hidden,
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

/// Ordena carpetas primero y luego por nombre sin distinguir mayúsculas.
pub fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Subcadena case-insensitive; vacío coincide con todo.
pub fn matches(name: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || name.to_lowercase().contains(&query)
}

/// Entradas que coinciden con el type-ahead, conservando el orden.
pub fn filter<'a>(entries: &'a [FileEntry], query: &str) -> Vec<&'a FileEntry> {
    entries
        .iter()
        .filter(|entry| matches(&entry.name, query))
        .collect()
}

/// Icono simbólico de 16 px para la fila.
pub fn icon_name(entry: &FileEntry) -> &'static str {
    match entry.kind {
        EntryKind::Dir | EntryKind::SymlinkToDir => "folder-symbolic",
        EntryKind::BrokenSymlink => "dialog-warning-symbolic",
        _ => match extension(&entry.name).as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" => {
                "image-x-generic-symbolic"
            }
            "rs" | "go" | "js" | "jsx" | "ts" | "tsx" | "py" | "c" | "h" | "cpp" | "hpp"
            | "php" | "html" | "css" | "vue" | "astro" | "json" | "toml" | "yaml" | "yml"
            | "md" | "sh" | "bash" | "zsh" | "fish" => "text-x-generic-symbolic",
            "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" => "package-x-generic-symbolic",
            "pdf" => "x-office-document-symbolic",
            "bin" | "exe" | "so" | "o" | "a" => "application-x-executable-symbolic",
            _ => "text-x-generic-symbolic",
        },
    }
}

/// Clase CSS para colorear el icono por tipo.
pub fn color_class(entry: &FileEntry) -> &'static str {
    match entry.kind {
        EntryKind::Dir | EntryKind::SymlinkToDir => "termust-folder",
        EntryKind::BrokenSymlink => "termust-broken",
        _ => match extension(&entry.name).as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" => "termust-image",
            "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" => "termust-archive",
            "bin" | "exe" | "so" | "o" | "a" => "termust-binary",
            "rs" | "go" | "js" | "jsx" | "ts" | "tsx" | "py" | "c" | "h" | "cpp" | "hpp"
            | "php" | "html" | "css" | "vue" | "astro" | "json" | "toml" | "yaml" | "yml"
            | "md" | "sh" | "bash" | "zsh" | "fish" => "termust-code",
            _ => "termust-file",
        },
    }
}

/// Etiqueta accesible del tipo de entrada.
pub fn type_label(entry: &FileEntry) -> &'static str {
    match entry.kind {
        EntryKind::Dir => "Carpeta",
        EntryKind::SymlinkToDir => "Enlace a carpeta",
        EntryKind::SymlinkToFile => "Enlace a archivo",
        EntryKind::BrokenSymlink => "Enlace roto",
        EntryKind::File => "Archivo",
        EntryKind::Other => "Otro",
    }
}

/// `access(2)` con R_OK|X_OK: un directorio sin permiso de apertura se marca
/// como no navegable sin intentar listarlo.
fn can_open_dir(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(c_path.as_ptr(), libc::R_OK | libc::X_OK) == 0 }
}

fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default()
}

fn map_error(path: &Path, error: &std::io::Error) -> FsError {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => FsError::NotFound(path.to_path_buf()),
        ErrorKind::NotADirectory => FsError::NotADirectory(path.to_path_buf()),
        ErrorKind::PermissionDenied => FsError::PermissionDenied(path.to_path_buf()),
        _ => FsError::Io(format!("{}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "termust-fs-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o700));
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn tree_sorted_folders_first() {
        let dir = TempDir::new("sort");
        fs::create_dir(dir.path().join("zeta")).unwrap();
        fs::create_dir(dir.path().join("Alpha")).unwrap();
        fs::write(dir.path().join("beta.txt"), b"b").unwrap();
        fs::write(dir.path().join("Alpha.txt"), b"a").unwrap();
        fs::write(dir.path().join(".oculto"), b"h").unwrap();
        let entries = list_dir(dir.path(), false).unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "zeta", "Alpha.txt", "beta.txt"]);
        assert!(entries[0].is_dir);
        assert!(!entries[2].is_dir);
    }

    #[test]
    fn hidden_entries_are_optional() {
        let dir = TempDir::new("hidden");
        fs::write(dir.path().join(".env"), b"x").unwrap();
        fs::write(dir.path().join("visible"), b"x").unwrap();
        assert_eq!(list_dir(dir.path(), false).unwrap().len(), 1);
        let all = list_dir(dir.path(), true).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|entry| entry.name == ".env" && entry.hidden));
    }

    #[test]
    fn files_broken_symlink_is_inert() {
        let dir = TempDir::new("symlink");
        std::os::unix::fs::symlink("/ruta/que/no/existe", dir.path().join("roto")).unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("a-directorio")).unwrap();
        let entries = list_dir(dir.path(), false).unwrap();
        let broken = entries
            .iter()
            .find(|entry| entry.name == "roto")
            .expect("symlink roto listado");
        assert_eq!(broken.kind, EntryKind::BrokenSymlink);
        assert!(!broken.is_dir);
        assert_eq!(icon_name(broken), "dialog-warning-symbolic");
        let directory = entries
            .iter()
            .find(|entry| entry.name == "a-directorio")
            .expect("symlink a directorio");
        assert_eq!(directory.kind, EntryKind::SymlinkToDir);
        assert!(directory.is_dir);
    }

    #[test]
    fn symlinked_directory_can_be_listed() {
        let dir = TempDir::new("symlink-root");
        fs::create_dir(dir.path().join("real")).unwrap();
        fs::write(dir.path().join("real/a.txt"), b"a").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        let entries = list_dir(&dir.path().join("link"), false).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.txt");
    }

    #[test]
    fn unreadable_dir_is_inert_row() {
        let dir = TempDir::new("unreadable-row");
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let entries = list_dir(dir.path(), false).unwrap();
        let locked_entry = entries
            .iter()
            .find(|entry| entry.name == "locked")
            .expect("locked listado");
        assert!(locked_entry.is_dir);
        assert!(!locked_entry.readable);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn permission_denied_is_error_not_panic() {
        let dir = TempDir::new("perm");
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let result = list_dir(&locked, false);
        // Como root no hay denegación; el resto debe ser un error tipado.
        if let Err(error) = &result {
            assert!(matches!(error, FsError::PermissionDenied(_)), "{error:?}");
        }
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            list_dir(&dir.path().join("no-existe"), false),
            Err(FsError::NotFound(_))
        ));
        let file = dir.path().join("archivo");
        fs::write(&file, b"x").unwrap();
        assert!(matches!(
            list_dir(&file, false),
            Err(FsError::NotADirectory(_))
        ));
    }

    #[test]
    fn tree_10k_entries_within_budget() {
        let dir = TempDir::new("10k");
        for index in 0..10_000 {
            fs::write(dir.path().join(format!("archivo-{index:05}.txt")), b"").unwrap();
        }
        let started = Instant::now();
        let entries = list_dir(dir.path(), false).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(entries.len(), 10_000);
        assert!(
            elapsed.as_millis() < 400,
            "listado 10k: {} ms (hard 400)",
            elapsed.as_millis()
        );
        eprintln!(
            "termust-fs: 10k listados en {:.1} ms (objetivo 150)",
            elapsed.as_secs_f64() * 1000.0
        );
    }

    #[test]
    fn files_search_typeahead() {
        let dir = TempDir::new("search");
        fs::write(dir.path().join("main.rs"), b"").unwrap();
        fs::write(dir.path().join("README.md"), b"").unwrap();
        let entries = list_dir(dir.path(), false).unwrap();
        let hits = filter(&entries, "MAIN");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "main.rs");
        assert_eq!(filter(&entries, "").len(), 2);
        assert!(matches("README.md", "read"));
        assert!(!matches("README.md", "rust"));
    }

    #[test]
    fn icons_and_colors_by_type() {
        let dir = TempDir::new("types");
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("main.rs"), b"").unwrap();
        fs::write(dir.path().join("foto.png"), b"").unwrap();
        fs::write(dir.path().join("paquete.tar"), b"").unwrap();
        fs::write(dir.path().join("notas"), b"").unwrap();
        let entries = list_dir(dir.path(), false).unwrap();
        let get = |name: &str| entries.iter().find(|entry| entry.name == name).unwrap();
        assert_eq!(icon_name(get("src")), "folder-symbolic");
        assert_eq!(color_class(get("src")), "termust-folder");
        assert_eq!(icon_name(get("main.rs")), "text-x-generic-symbolic");
        assert_eq!(color_class(get("main.rs")), "termust-code");
        assert_eq!(color_class(get("foto.png")), "termust-image");
        assert_eq!(color_class(get("paquete.tar")), "termust-archive");
        assert_eq!(color_class(get("notas")), "termust-file");
        assert_eq!(type_label(get("src")), "Carpeta");
    }
}
