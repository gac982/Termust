//! Bóveda de credenciales: SQLite + XChaCha20-Poly1305 con AAD por registro.
#![forbid(unsafe_code)]

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

const SCHEMA_VERSION: i64 = 1;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VaultError {
    Missing,
    Tampered,
    Invalid(String),
    Sql(String),
    Io(String),
    Crypto,
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::Missing => write!(f, "No hay credencial para ese origen"),
            VaultError::Tampered => write!(f, "La credencial está corrupta o fue alterada"),
            VaultError::Invalid(message) => write!(f, "{message}"),
            VaultError::Sql(message) => write!(f, "Error de base de datos: {message}"),
            VaultError::Io(message) => write!(f, "{message}"),
            VaultError::Crypto => write!(f, "Error criptográfico"),
        }
    }
}

impl std::error::Error for VaultError {}

/// Clave maestra de 32 B. No implementa `Debug`/`Display` y se borra al caer.
pub struct MasterKey([u8; KEY_LEN]);

impl MasterKey {
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    pub fn generate() -> Result<Self, VaultError> {
        let mut bytes = [0u8; KEY_LEN];
        getrandom::fill(&mut bytes).map_err(|error| VaultError::Io(error.to_string()))?;
        Ok(Self(bytes))
    }

    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(KEY_LEN * 2);
        for byte in self.0 {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    pub fn from_hex(value: &str) -> Result<Self, VaultError> {
        let value = value.trim();
        if value.len() != KEY_LEN * 2 {
            return Err(VaultError::Invalid(
                "clave maestra con longitud inválida".into(),
            ));
        }
        let mut bytes = [0u8; KEY_LEN];
        for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
            let text = std::str::from_utf8(chunk)
                .map_err(|_| VaultError::Invalid("clave no hexadecimal".into()))?;
            bytes[index] = u8::from_str_radix(text, 16)
                .map_err(|_| VaultError::Invalid("clave no hexadecimal".into()))?;
        }
        Ok(Self(bytes))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }

    #[cfg(test)]
    pub fn expose_for_test(&self) -> [u8; KEY_LEN] {
        self.0
    }
}

impl Drop for MasterKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Secreto en memoria sin `Debug`/`Display`; se borra al caer.
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    pub fn as_str(&self) -> Result<&str, VaultError> {
        std::str::from_utf8(&self.0)
            .map_err(|_| VaultError::Invalid("el secreto no es UTF-8".into()))
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub struct Credential {
    pub username: String,
    pub secret: Secret,
}

pub struct Vault {
    connection: Connection,
    key: MasterKey,
    path: PathBuf,
}

impl Vault {
    pub fn open(path: &Path, key: MasterKey) -> Result<Self, VaultError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| VaultError::Io(error.to_string()))?;
            set_mode(parent, 0o700)?;
        }
        let connection =
            Connection::open(path).map_err(|error| VaultError::Sql(error.to_string()))?;
        set_mode(path, 0o600)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 PRAGMA foreign_keys=ON;
                 CREATE TABLE IF NOT EXISTS meta (
                   k TEXT PRIMARY KEY,
                   v TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS credential (
                   id TEXT PRIMARY KEY,
                   origin TEXT NOT NULL UNIQUE,
                   username_ct BLOB NOT NULL,
                   secret_ct BLOB NOT NULL,
                   nonce BLOB NOT NULL,
                   aad_version INTEGER NOT NULL,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL,
                   UNIQUE(origin, id)
                 );",
            )
            .map_err(|error| VaultError::Sql(error.to_string()))?;
        connection
            .execute(
                "INSERT OR IGNORE INTO meta (k, v) VALUES ('schema_version', ?1)",
                [SCHEMA_VERSION.to_string()],
            )
            .map_err(|error| VaultError::Sql(error.to_string()))?;
        Ok(Self {
            connection,
            key,
            path: path.to_path_buf(),
        })
    }

    pub fn put(&mut self, origin: &str, username: &str, secret: &[u8]) -> Result<(), VaultError> {
        validate_origin(origin)?;
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|error| VaultError::Io(error.to_string()))?;
        let cipher = self.key.cipher();
        let username_ct = encrypt(
            &cipher,
            &nonce_field(&nonce, 0),
            &aad(origin, "username"),
            username.as_bytes(),
        )?;
        let secret_ct = encrypt(
            &cipher,
            &nonce_field(&nonce, 1),
            &aad(origin, "secret"),
            secret,
        )?;
        let now = now_seconds();
        self.connection
            .execute(
                "INSERT INTO credential
                   (id, origin, username_ct, secret_ct, nonce, aad_version, created_at, updated_at)
                 VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT(origin) DO UPDATE SET
                   username_ct = excluded.username_ct,
                   secret_ct = excluded.secret_ct,
                   nonce = excluded.nonce,
                   aad_version = excluded.aad_version,
                   updated_at = excluded.updated_at",
                params![
                    origin,
                    username_ct,
                    secret_ct,
                    nonce.to_vec(),
                    SCHEMA_VERSION,
                    now
                ],
            )
            .map_err(|error| VaultError::Sql(error.to_string()))?;
        Ok(())
    }

    pub fn get(&self, origin: &str) -> Result<Credential, VaultError> {
        let row = self
            .connection
            .query_row(
                "SELECT username_ct, secret_ct, nonce FROM credential WHERE origin = ?1",
                [origin],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| VaultError::Sql(error.to_string()))?;
        let Some((username_ct, secret_ct, row_nonce)) = row else {
            return Err(VaultError::Missing);
        };
        if row_nonce.len() != NONCE_LEN {
            return Err(VaultError::Tampered);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&row_nonce);
        let cipher = self.key.cipher();
        let username = decrypt(
            &cipher,
            &nonce_field(&nonce, 0),
            &aad(origin, "username"),
            &username_ct,
        )?;
        let secret = decrypt(
            &cipher,
            &nonce_field(&nonce, 1),
            &aad(origin, "secret"),
            &secret_ct,
        )?;
        let username = String::from_utf8(username).map_err(|_| VaultError::Tampered)?;
        Ok(Credential {
            username,
            secret: Secret(secret),
        })
    }

    pub fn delete(&mut self, origin: &str) -> Result<bool, VaultError> {
        let changed = self
            .connection
            .execute(
                "UPDATE credential
                 SET username_ct = randomblob(length(username_ct)),
                     secret_ct = randomblob(length(secret_ct))
                 WHERE origin = ?1",
                [origin],
            )
            .map_err(|error| VaultError::Sql(error.to_string()))?;
        if changed == 0 {
            return Ok(false);
        }
        self.connection
            .execute("DELETE FROM credential WHERE origin = ?1", [origin])
            .map_err(|error| VaultError::Sql(error.to_string()))?;
        self.checkpoint()?;
        Ok(true)
    }

    pub fn list_origins(&self) -> Result<Vec<String>, VaultError> {
        let mut statement = self
            .connection
            .prepare("SELECT origin FROM credential ORDER BY origin")
            .map_err(|error| VaultError::Sql(error.to_string()))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| VaultError::Sql(error.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| VaultError::Sql(error.to_string()))
    }

    pub fn checkpoint(&self) -> Result<(), VaultError> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|error| VaultError::Sql(error.to_string()))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Deriva una clave maestra desde una passphrase con Argon2id (m=64 MiB,
/// t=3, p=1). El salt debe persistirse fuera de la bóveda.
pub fn derive_key(passphrase: &str, salt: &[u8]) -> Result<MasterKey, VaultError> {
    if salt.len() < 8 {
        return Err(VaultError::Invalid("salt demasiado corto".into()));
    }
    let params = argon2::ParamsBuilder::new()
        .m_cost(64 * 1024)
        .t_cost(3)
        .p_cost(1)
        .output_len(KEY_LEN)
        .build()
        .map_err(|_| VaultError::Crypto)?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = [0u8; KEY_LEN];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|_| VaultError::Crypto)?;
    Ok(MasterKey(out))
}

fn validate_origin(origin: &str) -> Result<(), VaultError> {
    if origin.trim().is_empty() || origin.chars().any(|ch| ch.is_control()) || origin.contains('\0')
    {
        return Err(VaultError::Invalid("origen inválido".into()));
    }
    Ok(())
}

fn aad(origin: &str, field: &str) -> Vec<u8> {
    format!("{SCHEMA_VERSION}\0{origin}\0{field}").into_bytes()
}

fn nonce_field(base: &[u8; NONCE_LEN], field: u8) -> [u8; NONCE_LEN] {
    let mut nonce = *base;
    nonce[NONCE_LEN - 1] ^= field;
    nonce
}

fn encrypt(
    cipher: &XChaCha20Poly1305,
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, VaultError> {
    let nonce = XNonce::try_from(nonce.as_slice()).map_err(|_| VaultError::Crypto)?;
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| VaultError::Crypto)
}

fn decrypt(
    cipher: &XChaCha20Poly1305,
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, VaultError> {
    let nonce = XNonce::try_from(nonce.as_slice()).map_err(|_| VaultError::Crypto)?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| VaultError::Tampered)
}

fn set_mode(path: &Path, mode: u32) -> Result<(), VaultError> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|error| VaultError::Io(error.to_string()))
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "termust-vault-{label}-{}-{}",
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
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn key(byte: u8) -> MasterKey {
        MasterKey::from_bytes([byte; 32])
    }

    #[test]
    fn vault_roundtrip_and_origin_isolation() {
        let dir = TempDir::new("roundtrip");
        let path = dir.path().join("vault.sqlite");
        let mut vault = Vault::open(&path, key(7)).expect("open");
        vault
            .put("https://github.com", "alice", b"token-123")
            .expect("put");
        let stored = vault.get("https://github.com").expect("get");
        assert_eq!(stored.username, "alice");
        assert_eq!(stored.secret.expose(), b"token-123");
        assert!(matches!(
            vault.get("https://github.com.evil.tld"),
            Err(VaultError::Missing)
        ));
        assert!(matches!(
            vault.get("http://github.com"),
            Err(VaultError::Missing)
        ));
        assert_eq!(
            vault.list_origins().unwrap(),
            vec!["https://github.com".to_string()]
        );
        vault
            .put("https://github.com", "alice", b"token-456")
            .expect("rotate");
        assert_eq!(
            vault.get("https://github.com").unwrap().secret.expose(),
            b"token-456"
        );
    }

    #[test]
    fn vault_tamper_detected() {
        let dir = TempDir::new("tamper");
        let path = dir.path().join("vault.sqlite");
        let mut vault = Vault::open(&path, key(9)).expect("open");
        vault
            .put("https://gitlab.com", "bob", b"secret-value")
            .expect("put");
        drop(vault);
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection
                .execute(
                    "UPDATE credential SET secret_ct = zeroblob(length(secret_ct) + 1) WHERE origin = ?1",
                    ["https://gitlab.com"],
                )
                .unwrap();
        }
        let vault = Vault::open(&path, key(9)).expect("reopen");
        let result = vault.get("https://gitlab.com");
        assert!(matches!(result, Err(VaultError::Tampered)));
    }

    #[test]
    fn vault_plaintext_never_on_disk() {
        let dir = TempDir::new("plaintext");
        let path = dir.path().join("vault.sqlite");
        let mut vault = Vault::open(&path, key(3)).expect("open");
        vault
            .put("https://example.com", "user", b"PLAINTEXT-SENTINEL-42")
            .expect("put");
        drop(vault);
        let bytes = fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows(b"PLAINTEXT-SENTINEL-42".len())
                .any(|window| window == b"PLAINTEXT-SENTINEL-42"),
            "el secreto quedó en claro en el archivo"
        );
        let wal = path.with_extension("sqlite-wal");
        if let Ok(bytes) = fs::read(&wal) {
            assert!(!bytes
                .windows(b"PLAINTEXT-SENTINEL-42".len())
                .any(|window| window == b"PLAINTEXT-SENTINEL-42"));
        }
    }

    #[test]
    fn vault_wrong_key_and_transplant_rejected() {
        let dir = TempDir::new("aad");
        let path = dir.path().join("vault.sqlite");
        let mut vault = Vault::open(&path, key(5)).expect("open");
        vault
            .put("https://one.example", "u", b"uno")
            .expect("put one");
        vault
            .put("https://two.example", "u", b"dos")
            .expect("put two");
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            let (ct, nonce): (Vec<u8>, Vec<u8>) = connection
                .query_row(
                    "SELECT secret_ct, nonce FROM credential WHERE origin = ?1",
                    ["https://one.example"],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE credential SET secret_ct = ?1, nonce = ?2 WHERE origin = ?3",
                    rusqlite::params![ct, nonce, "https://two.example"],
                )
                .unwrap();
        }
        let vault = Vault::open(&path, key(5)).expect("reopen");
        assert!(matches!(
            vault.get("https://two.example"),
            Err(VaultError::Tampered)
        ));
        let wrong = Vault::open(&path, key(6)).expect("open wrong key");
        assert!(matches!(
            wrong.get("https://one.example"),
            Err(VaultError::Tampered)
        ));
    }

    #[test]
    fn vault_delete_removes_and_overwrites() {
        let dir = TempDir::new("delete");
        let path = dir.path().join("vault.sqlite");
        let mut vault = Vault::open(&path, key(4)).expect("open");
        vault
            .put("https://delete.example", "u", b"BORRAR-SENTINEL-7")
            .expect("put");
        assert!(vault.delete("https://delete.example").expect("delete"));
        assert!(matches!(
            vault.get("https://delete.example"),
            Err(VaultError::Missing)
        ));
        vault.checkpoint().unwrap();
        drop(vault);
        let bytes = fs::read(&path).unwrap();
        assert!(!bytes
            .windows(b"BORRAR-SENTINEL-7".len())
            .any(|window| window == b"BORRAR-SENTINEL-7"));
    }

    #[test]
    fn vault_file_permissions_0600() {
        let dir = TempDir::new("perm");
        let path = dir.path().join("vault.sqlite");
        let _vault = Vault::open(&path, key(1)).expect("open");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "modo {mode:o}");
        let dir_mode = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "modo dir {dir_mode:o}");
    }

    #[test]
    fn vault_passphrase_derivation_is_stable() {
        let salt = [42u8; 16];
        let a = derive_key("correct horse battery staple", &salt).unwrap();
        let b = derive_key("correct horse battery staple", &salt).unwrap();
        assert_eq!(a.expose_for_test(), b.expose_for_test());
        assert_ne!(
            a.expose_for_test(),
            derive_key("otra", &salt).unwrap().expose_for_test()
        );
    }
}
