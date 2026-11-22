use crate::terminal::Size;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub argv: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
    pub size: Size,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PtyEvent {
    Output(Vec<u8>),
    Exited(Option<i32>),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyError {
    Spawn(String),
    Io(String),
}

impl std::fmt::Display for PtyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PtyError::Spawn(e) => write!(f, "spawn failed: {e}"),
            PtyError::Io(e) => write!(f, "io failed: {e}"),
        }
    }
}

impl std::error::Error for PtyError {}

pub trait PtySession: Send {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    fn resize(&mut self, size: Size) -> std::io::Result<()>;
    fn kill(&mut self) -> std::io::Result<()>;
    fn pid(&self) -> u32;

    /// ¿Hay un proceso en foreground distinto del shell? (FR-1.4)
    fn has_foreground_job(&self) -> bool {
        false
    }
}

pub struct PtyHandle {
    pub session: Box<dyn PtySession>,
    pub events: Receiver<PtyEvent>,
}

pub trait PtySpawner: Send + Sync {
    fn spawn(&self, spec: SpawnSpec) -> Result<PtyHandle, PtyError>;
}
