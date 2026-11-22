use std::path::PathBuf;

pub type TabId = u64;
pub type PaneId = u64;

/// Árbol de paneles. En M3 solo hay una hoja; M4 agrega `Split`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneTree {
    Leaf(PaneId),
}

impl PaneTree {
    pub fn leaves(&self) -> Vec<PaneId> {
        match self {
            PaneTree::Leaf(id) => vec![*id],
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        self.leaves().contains(&id)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Activity {
    #[default]
    Idle,
    Active,
    WaitingInput,
    Exited(Option<i32>),
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub layout: PaneTree,
    pub active_pane: PaneId,
    pub title_override: Option<String>,
    pub last_osc_title: Option<String>,
    pub cwd: PathBuf,
    pub theme_id: Option<String>,
    pub activity: Activity,
}

#[derive(Clone, Debug)]
pub struct TabModel {
    tabs: Vec<Tab>,
    active: TabId,
    next_tab: TabId,
    next_pane: PaneId,
}

impl TabModel {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        let mut model = Self {
            tabs: Vec::new(),
            active: 0,
            next_tab: 1,
            next_pane: 1,
        };
        let id = model.alloc(cwd.into());
        model.active = id;
        model
    }

    fn alloc(&mut self, cwd: PathBuf) -> TabId {
        let id = self.next_tab;
        self.next_tab += 1;
        let pane = self.next_pane;
        self.next_pane += 1;
        self.tabs.push(Tab {
            id,
            layout: PaneTree::Leaf(pane),
            active_pane: pane,
            title_override: None,
            last_osc_title: None,
            cwd,
            theme_id: None,
            activity: Activity::Idle,
        });
        id
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_id(&self) -> TabId {
        self.active
    }

    pub fn active(&self) -> &Tab {
        self.get(self.active).expect("active tab invariant")
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn new_tab(&mut self, cwd: impl Into<PathBuf>) -> TabId {
        let id = self.alloc(cwd.into());
        self.active = id;
        id
    }

    pub fn duplicate(&mut self, id: TabId) -> Option<TabId> {
        let source = self.get(id)?.clone();
        let new_id = self.alloc(source.cwd);
        let new = self.get_mut(new_id)?;
        new.title_override = source.title_override;
        new.last_osc_title = source.last_osc_title;
        new.theme_id = source.theme_id;
        self.active = new_id;
        Some(new_id)
    }

    /// Cierra una pestaña y devuelve el id activo resultante.
    pub fn close(&mut self, id: TabId) -> Option<TabId> {
        let pos = self.tabs.iter().position(|t| t.id == id)?;
        let removed = self.tabs.remove(pos);
        if self.tabs.is_empty() {
            let fresh = self.alloc(removed.cwd);
            self.active = fresh;
            return Some(fresh);
        }
        if self.active == id {
            let next = self
                .tabs
                .get(pos)
                .or_else(|| self.tabs.get(pos.saturating_sub(1)))
                .map(|t| t.id)
                .expect("non-empty invariant");
            self.active = next;
        }
        Some(self.active)
    }

    pub fn activate(&mut self, id: TabId) -> bool {
        if self.get(id).is_some() {
            self.active = id;
            true
        } else {
            false
        }
    }

    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        if from >= self.tabs.len() {
            return false;
        }
        let to = to.min(self.tabs.len() - 1);
        if from == to {
            return true;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        true
    }

    pub fn move_active(&mut self, delta: isize) -> bool {
        let from = match self.tabs.iter().position(|t| t.id == self.active) {
            Some(p) => p,
            None => return false,
        };
        let to = (from as isize + delta).clamp(0, self.tabs.len() as isize - 1) as usize;
        self.move_tab(from, to)
    }

    pub fn rename(&mut self, id: TabId, name: Option<String>) -> bool {
        match self.get_mut(id) {
            Some(tab) => {
                tab.title_override = name.filter(|s| !s.trim().is_empty());
                true
            }
            None => false,
        }
    }

    pub fn set_osc_title(&mut self, id: TabId, title: impl Into<String>) -> bool {
        match self.get_mut(id) {
            Some(tab) => {
                tab.last_osc_title = Some(title.into());
                true
            }
            None => false,
        }
    }

    pub fn set_cwd(&mut self, id: TabId, cwd: impl Into<PathBuf>) -> bool {
        match self.get_mut(id) {
            Some(tab) => {
                tab.cwd = cwd.into();
                true
            }
            None => false,
        }
    }

    pub fn set_activity(&mut self, id: TabId, activity: Activity) -> bool {
        match self.get_mut(id) {
            Some(tab) => {
                tab.activity = activity;
                true
            }
            None => false,
        }
    }

    pub fn pane_ids(&self, id: TabId) -> Option<Vec<PaneId>> {
        self.get(id).map(|t| t.layout.leaves())
    }

    pub fn contains_pane(&self, pane: PaneId) -> bool {
        self.tabs.iter().any(|t| t.layout.contains(pane))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_one_active_tab() {
        let m = TabModel::new("/tmp");
        assert_eq!(m.len(), 1);
        assert_eq!(m.active().cwd, PathBuf::from("/tmp"));
        assert_eq!(m.active().layout.leaves().len(), 1);
    }

    #[test]
    fn new_tab_appends_and_activates() {
        let mut m = TabModel::new("/a");
        let first = m.active_id();
        let id = m.new_tab("/b");
        assert_eq!(m.len(), 2);
        assert_eq!(m.active_id(), id);
        assert_eq!(m.tabs()[1].id, id);
        assert_ne!(first, id);
    }

    #[test]
    fn close_last_creates_fresh_same_cwd() {
        let mut m = TabModel::new("/a");
        let id = m.active_id();
        let new_active = m.close(id).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m.active_id(), new_active);
        assert_eq!(m.active().cwd, PathBuf::from("/a"));
        assert_ne!(new_active, id);
    }

    #[test]
    fn close_active_activates_neighbor() {
        let mut m = TabModel::new("/a");
        let a = m.active_id();
        let b = m.new_tab("/b");
        let c = m.new_tab("/c");
        m.activate(b);
        assert_eq!(m.close(b), Some(c));
        assert_eq!(m.active_id(), c);
        m.activate(a);
        assert_eq!(m.close(a), Some(c));
    }

    #[test]
    fn close_inactive_keeps_active() {
        let mut m = TabModel::new("/a");
        let a = m.active_id();
        let b = m.new_tab("/b");
        assert_eq!(m.close(a), Some(b));
        assert_eq!(m.active_id(), b);
    }

    #[test]
    fn move_preserves_active_id() {
        let mut m = TabModel::new("/a");
        let b = m.new_tab("/b");
        let _c = m.new_tab("/c");
        m.activate(b);
        assert!(m.move_tab(2, 0));
        assert_eq!(m.active_id(), b);
        assert_eq!(m.tabs()[0].id, m.tabs()[0].id);
    }

    #[test]
    fn rename_override_and_clear() {
        let mut m = TabModel::new("/a");
        let id = m.active_id();
        assert!(m.rename(id, Some("build".into())));
        assert_eq!(m.get(id).unwrap().title_override.as_deref(), Some("build"));
        assert!(m.rename(id, Some("   ".into())));
        assert!(m.get(id).unwrap().title_override.is_none());
    }

    #[test]
    fn duplicate_copies_context() {
        let mut m = TabModel::new("/a");
        let id = m.active_id();
        m.rename(id, Some("x".into()));
        let dup = m.duplicate(id).unwrap();
        assert_eq!(m.active_id(), dup);
        assert_eq!(m.get(dup).unwrap().cwd, PathBuf::from("/a"));
        assert_eq!(m.get(dup).unwrap().title_override.as_deref(), Some("x"));
        assert_ne!(
            m.get(dup).unwrap().active_pane,
            m.get(id).unwrap().active_pane
        );
    }

    #[test]
    fn pane_ids_and_containment() {
        let m = TabModel::new("/a");
        let panes = m.pane_ids(m.active_id()).unwrap();
        assert_eq!(panes.len(), 1);
        assert!(m.contains_pane(panes[0]));
        assert!(!m.contains_pane(999));
    }
}
