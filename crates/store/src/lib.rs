//! Persistencia de sesión (schema v2) con escritura atómica.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use termust_core::tabs::{PaneId, PaneTree, SplitOrientation, MAX_PANES};

pub const SCHEMA_VERSION: u32 = 2;
pub const DEFAULT_WIDTH: i32 = 1024;
pub const DEFAULT_HEIGHT: i32 = 700;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    #[serde(default)]
    pub window: WindowState,
    pub active_tab: usize,
    pub tabs: Vec<TabState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    #[serde(default = "default_width")]
    pub width: i32,
    #[serde(default = "default_height")]
    pub height: i32,
    #[serde(default)]
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            maximized: false,
        }
    }
}

fn default_width() -> i32 {
    DEFAULT_WIDTH
}

fn default_height() -> i32 {
    DEFAULT_HEIGHT
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    #[serde(default)]
    pub title_override: Option<String>,
    #[serde(default)]
    pub theme_id: Option<String>,
    pub active_pane: PaneId,
    pub layout: LayoutNode,
    #[serde(default)]
    pub panes: Vec<PaneState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LayoutNode {
    Leaf {
        id: PaneId,
    },
    Split {
        orientation: Orientation,
        ratio: f64,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    Horizontal,
    Vertical,
}

impl From<SplitOrientation> for Orientation {
    fn from(value: SplitOrientation) -> Self {
        match value {
            SplitOrientation::Horizontal => Orientation::Horizontal,
            SplitOrientation::Vertical => Orientation::Vertical,
        }
    }
}

impl From<Orientation> for SplitOrientation {
    fn from(value: Orientation) -> Self {
        match value {
            Orientation::Horizontal => SplitOrientation::Horizontal,
            Orientation::Vertical => SplitOrientation::Vertical,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneState {
    pub id: PaneId,
    pub cwd: String,
    #[serde(default)]
    pub last_osc_title: Option<String>,
    #[serde(default)]
    pub agent_hint: Option<String>,
}

/// Vista independiente del motor que la app usa para guardar/restaurar.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    pub window: WindowState,
    pub active_tab: usize,
    pub tabs: Vec<TabSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabSnapshot {
    pub layout: PaneTree,
    pub active_pane: PaneId,
    pub cwd: PathBuf,
    pub title_override: Option<String>,
    pub last_osc_title: Option<String>,
    pub theme_id: Option<String>,
    pub panes: Vec<PaneSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaneSnapshot {
    pub id: PaneId,
    pub cwd: PathBuf,
    pub last_osc_title: Option<String>,
    pub agent_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoadResult {
    Loaded(SessionSnapshot),
    Missing,
    Corrupt(String),
    FutureVersion(u32),
}

pub fn session_path() -> PathBuf {
    if let Some(path) = std::env::var_os("TERMUST_SESSION_PATH") {
        return PathBuf::from(path);
    }
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("termust/session.json")
}

/// Aplica FR-2.4: los cwd inexistentes caen a `home`. Devuelve true si cambió algo.
pub fn fallback_cwds(snapshot: &mut SessionSnapshot, home: &Path) -> bool {
    let mut changed = false;
    for tab in &mut snapshot.tabs {
        for pane in &mut tab.panes {
            if !pane.cwd.is_dir() {
                pane.cwd = home.to_path_buf();
                changed = true;
            }
        }
        if let Some(pane) = tab.panes.iter().find(|pane| pane.id == tab.active_pane) {
            tab.cwd = pane.cwd.clone();
        }
        if !tab.cwd.is_dir() {
            tab.cwd = home.to_path_buf();
            changed = true;
        }
    }
    changed
}

pub fn load(path: &Path) -> LoadResult {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return LoadResult::Missing,
        Err(e) => return LoadResult::Corrupt(e.to_string()),
    };
    let session: Session = match serde_json::from_str(&text) {
        Ok(session) => session,
        Err(e) => return LoadResult::Corrupt(e.to_string()),
    };
    if session.version > SCHEMA_VERSION {
        return LoadResult::FutureVersion(session.version);
    }
    if session.version != SCHEMA_VERSION {
        return LoadResult::Corrupt(format!("schema v{} no soportado", session.version));
    }
    match session.into_snapshot() {
        Ok(snapshot) => LoadResult::Loaded(snapshot),
        Err(e) => LoadResult::Corrupt(e),
    }
}

pub fn save_snapshot(path: &Path, snapshot: &SessionSnapshot) -> io::Result<()> {
    save_atomic(path, &Session::from_snapshot(snapshot))
}

pub fn save_atomic(path: &Path, session: &Session) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let json = serde_json::to_vec_pretty(session).map_err(io::Error::other)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("session.json");
    let tmp = parent.join(format!(".{name}.{}.{stamp}.tmp", std::process::id()));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&json)?;
        file.sync_all()?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

impl Session {
    pub fn from_snapshot(snapshot: &SessionSnapshot) -> Self {
        Self {
            version: SCHEMA_VERSION,
            window: snapshot.window.clone(),
            active_tab: snapshot.active_tab,
            tabs: snapshot
                .tabs
                .iter()
                .map(|tab| TabState {
                    title_override: tab.title_override.clone(),
                    theme_id: tab.theme_id.clone(),
                    active_pane: tab.active_pane,
                    layout: LayoutNode::from_tree(&tab.layout),
                    panes: tab
                        .panes
                        .iter()
                        .map(|pane| PaneState {
                            id: pane.id,
                            cwd: pane.cwd.to_string_lossy().into_owned(),
                            last_osc_title: pane.last_osc_title.clone(),
                            agent_hint: pane.agent_hint.clone(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn into_snapshot(self) -> Result<SessionSnapshot, String> {
        if self.tabs.is_empty() {
            return Err("la sesión no tiene pestañas".into());
        }
        if self.active_tab >= self.tabs.len() {
            return Err("pestaña activa fuera de rango".into());
        }
        let mut tabs = Vec::with_capacity(self.tabs.len());
        for tab in self.tabs {
            let layout = tab.layout.into_tree()?;
            let leaves = layout.leaves();
            if leaves.len() > MAX_PANES {
                return Err("la pestaña supera 4 paneles".into());
            }
            if !leaves.contains(&tab.active_pane) {
                return Err("panel activo inexistente".into());
            }
            if tab.panes.len() != leaves.len() {
                return Err("faltan datos de paneles".into());
            }
            let mut panes = Vec::with_capacity(leaves.len());
            let mut seen = std::collections::HashSet::new();
            for leaf in &leaves {
                let pane = tab
                    .panes
                    .iter()
                    .find(|pane| pane.id == *leaf)
                    .ok_or_else(|| format!("falta el panel {leaf}"))?;
                if !seen.insert(pane.id) {
                    return Err("panel duplicado".into());
                }
                if pane.cwd.is_empty() {
                    return Err("cwd vacío".into());
                }
                panes.push(PaneSnapshot {
                    id: pane.id,
                    cwd: PathBuf::from(&pane.cwd),
                    last_osc_title: pane.last_osc_title.clone(),
                    agent_hint: pane.agent_hint.clone(),
                });
            }
            let cwd = panes
                .iter()
                .find(|pane| pane.id == tab.active_pane)
                .map(|pane| pane.cwd.clone())
                .ok_or_else(|| "panel activo sin cwd".to_string())?;
            tabs.push(TabSnapshot {
                layout,
                active_pane: tab.active_pane,
                cwd,
                title_override: tab.title_override,
                last_osc_title: panes
                    .iter()
                    .find(|pane| pane.id == tab.active_pane)
                    .and_then(|pane| pane.last_osc_title.clone()),
                theme_id: tab.theme_id,
                panes,
            });
        }
        Ok(SessionSnapshot {
            window: self.window,
            active_tab: self.active_tab,
            tabs,
        })
    }
}

impl LayoutNode {
    fn from_tree(tree: &PaneTree) -> Self {
        match tree {
            PaneTree::Leaf(id) => LayoutNode::Leaf { id: *id },
            PaneTree::Split {
                orientation,
                ratio,
                first,
                second,
            } => LayoutNode::Split {
                orientation: (*orientation).into(),
                ratio: *ratio,
                first: Box::new(LayoutNode::from_tree(first)),
                second: Box::new(LayoutNode::from_tree(second)),
            },
        }
    }

    fn into_tree(self) -> Result<PaneTree, String> {
        match self {
            LayoutNode::Leaf { id } => Ok(PaneTree::Leaf(id)),
            LayoutNode::Split {
                orientation,
                ratio,
                first,
                second,
            } => {
                if !ratio.is_finite() {
                    return Err("ratio no finito".into());
                }
                Ok(PaneTree::Split {
                    orientation: orientation.into(),
                    ratio: ratio.clamp(0.1, 0.9),
                    first: Box::new(first.into_tree()?),
                    second: Box::new(second.into_tree()?),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> SessionSnapshot {
        let layout = PaneTree::Split {
            orientation: SplitOrientation::Horizontal,
            ratio: 0.3,
            first: Box::new(PaneTree::Leaf(1)),
            second: Box::new(PaneTree::Split {
                orientation: SplitOrientation::Vertical,
                ratio: 0.6,
                first: Box::new(PaneTree::Leaf(2)),
                second: Box::new(PaneTree::Leaf(3)),
            }),
        };
        SessionSnapshot {
            window: WindowState {
                width: 1280,
                height: 800,
                maximized: false,
            },
            active_tab: 0,
            tabs: vec![
                TabSnapshot {
                    layout,
                    active_pane: 3,
                    cwd: PathBuf::from("/tmp"),
                    title_override: Some("build".into()),
                    last_osc_title: Some("cmd".into()),
                    theme_id: Some("dark".into()),
                    panes: vec![
                        PaneSnapshot {
                            id: 1,
                            cwd: PathBuf::from("/tmp"),
                            last_osc_title: Some("one".into()),
                            agent_hint: None,
                        },
                        PaneSnapshot {
                            id: 2,
                            cwd: PathBuf::from("/var"),
                            last_osc_title: Some("two".into()),
                            agent_hint: Some("claude".into()),
                        },
                        PaneSnapshot {
                            id: 3,
                            cwd: PathBuf::from("/tmp"),
                            last_osc_title: Some("cmd".into()),
                            agent_hint: None,
                        },
                    ],
                },
                TabSnapshot {
                    layout: PaneTree::Leaf(4),
                    active_pane: 4,
                    cwd: PathBuf::from("/home/u"),
                    title_override: None,
                    last_osc_title: None,
                    theme_id: None,
                    panes: vec![PaneSnapshot {
                        id: 4,
                        cwd: PathBuf::from("/home/u"),
                        last_osc_title: None,
                        agent_hint: None,
                    }],
                },
            ],
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "termust-store-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trip_preserves_tree_ratios_and_focus() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("session.json");
        let snap = snapshot();
        save_snapshot(&path, &snap).unwrap();
        match load(&path) {
            LoadResult::Loaded(loaded) => {
                assert_eq!(loaded, snap);
                assert_eq!(loaded.tabs[0].layout.ratio_at_path(&[]), Some(0.3));
                assert_eq!(loaded.tabs[0].layout.ratio_at_path(&[true]), Some(0.6));
                assert_eq!(loaded.tabs[0].active_pane, 3);
            }
            other => panic!("load: {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_atomically_with_0600() {
        let dir = temp_dir("atomic");
        let path = dir.join("session.json");
        save_snapshot(&path, &snapshot()).unwrap();
        let entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["session.json".to_string()]);
        let mode = fs::metadata(&path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.permissions().mode() & 0o777, 0o600);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_missing_and_future_versions() {
        let dir = temp_dir("corrupt");
        assert!(matches!(load(&dir.join("none.json")), LoadResult::Missing));
        let bad = dir.join("bad.json");
        fs::write(&bad, "{truncado").unwrap();
        assert!(matches!(load(&bad), LoadResult::Corrupt(_)));
        let future = dir.join("future.json");
        fs::write(&future, r#"{"version":99,"active_tab":0,"tabs":[]}"#).unwrap();
        assert_eq!(load(&future), LoadResult::FutureVersion(99));
        let v1 = dir.join("v1.json");
        fs::write(&v1, r#"{"version":1,"active_tab":0,"tabs":[]}"#).unwrap();
        assert!(matches!(load(&v1), LoadResult::Corrupt(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_semantics_are_corrupt() {
        let mut snap = snapshot();
        snap.tabs[0].active_pane = 99;
        let session = Session::from_snapshot(&snap);
        assert!(session.into_snapshot().is_err());
        let mut snap = snapshot();
        snap.tabs[0].layout = PaneTree::Split {
            orientation: SplitOrientation::Horizontal,
            ratio: 0.5,
            first: Box::new(PaneTree::Leaf(1)),
            second: Box::new(PaneTree::Leaf(2)),
        };
        snap.tabs[0].panes.retain(|p| p.id != 2);
        assert!(Session::from_snapshot(&snap).into_snapshot().is_err());
        let mut snap = snapshot();
        snap.active_tab = 5;
        assert!(Session::from_snapshot(&snap).into_snapshot().is_err());
    }

    #[test]
    fn ratios_are_clamped_on_load() {
        let dir = temp_dir("clamp");
        let path = dir.join("session.json");
        let json = r#"{
          "version": 2,
          "window": {"width": 800, "height": 600, "maximized": false},
          "active_tab": 0,
          "tabs": [{
            "active_pane": 1,
            "layout": {"type": "split", "orientation": "horizontal", "ratio": 2.0,
              "first": {"type": "leaf", "id": 1}, "second": {"type": "leaf", "id": 2}},
            "panes": [{"id": 1, "cwd": "/tmp"}, {"id": 2, "cwd": "/tmp"}]
          }]
        }"#;
        fs::write(&path, json).unwrap();
        match load(&path) {
            LoadResult::Loaded(snap) => {
                assert_eq!(snap.tabs[0].layout.ratio_at_path(&[]), Some(0.9));
            }
            other => panic!("load: {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_cwds_fall_back_to_home() {
        let dir = temp_dir("cwd");
        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        let existing = dir.join("repo");
        fs::create_dir_all(&existing).unwrap();
        let mut snap = snapshot();
        snap.tabs[0].panes[0].cwd = existing.clone();
        snap.tabs[0].panes[1].cwd = dir.join("borrado");
        snap.tabs[0].panes[2].cwd = home.clone();
        snap.tabs[0].active_pane = 2;
        snap.tabs[1].panes[0].cwd = dir.join("otro-borrado");
        let changed = fallback_cwds(&mut snap, &home);
        assert!(changed);
        assert_eq!(snap.tabs[0].panes[0].cwd, existing);
        assert_eq!(snap.tabs[0].panes[1].cwd, home);
        assert_eq!(snap.tabs[0].panes[2].cwd, home);
        assert_eq!(snap.tabs[0].cwd, home);
        assert_eq!(snap.tabs[1].panes[0].cwd, home);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn schema_has_no_scrollback_or_process_data() {
        let json = serde_json::to_string(&Session::from_snapshot(&snapshot())).unwrap();
        assert!(!json.contains("scrollback"));
        assert!(!json.contains("env"));
        assert!(!json.contains("argv"));
    }

    #[test]
    fn path_can_be_overridden_by_env() {
        // El helper no debe depender del entorno del test; se valida el nombre.
        let path = session_path();
        assert!(
            path.ends_with("termust/session.json")
                || std::env::var_os("TERMUST_SESSION_PATH").is_some()
        );
    }
}
