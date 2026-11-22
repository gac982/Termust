use std::collections::HashMap;
use std::path::PathBuf;

pub type TabId = u64;
pub type PaneId = u64;
pub const MAX_PANES: usize = 4;
pub const MIN_PANE_WIDTH_PX: f64 = 320.0;

#[derive(Clone, Debug)]
pub struct RestoreTab {
    pub layout: PaneTree,
    pub active_pane: PaneId,
    pub cwd: PathBuf,
    pub title_override: Option<String>,
    pub last_osc_title: Option<String>,
    pub theme_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreError {
    Empty,
    InvalidActiveTab,
    InvalidActivePane,
    TooManyPanes,
    DuplicatePane,
}

/// El ancho disponible debe alcanzar para `columns` paneles de 320 px.
pub fn fits_min_width(available_px: f64, columns: usize) -> bool {
    if columns == 0 || available_px <= 0.0 {
        return true;
    }
    available_px / columns as f64 + 0.5 >= MIN_PANE_WIDTH_PX
}
const MIN_RATIO: f64 = 0.1;
const MAX_RATIO: f64 = 0.9;
const DEFAULT_RATIO: f64 = 0.5;

/// `Horizontal` reparte izquierda/derecha (divisor vertical); `Vertical`
/// reparte arriba/abajo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitOrientation {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitError {
    MaxPanes,
    UnknownPane,
    LastPane,
    TooManyPanes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    TwoColumns,
    TwoRows,
    ThreeColumns,
    Grid2x2,
}

impl Preset {
    pub fn panes(self) -> usize {
        match self {
            Preset::TwoColumns | Preset::TwoRows => 2,
            Preset::ThreeColumns => 3,
            Preset::Grid2x2 => 4,
        }
    }

    pub fn columns(self) -> usize {
        match self {
            Preset::TwoColumns | Preset::Grid2x2 => 2,
            Preset::ThreeColumns => 3,
            Preset::TwoRows => 1,
        }
    }

    pub fn build(self, ids: &[PaneId]) -> PaneTree {
        let leaf = |i: usize| PaneTree::Leaf(ids[i]);
        let split = |orientation, ratio, first, second| PaneTree::Split {
            orientation,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        };
        match self {
            Preset::TwoColumns => split(
                SplitOrientation::Horizontal,
                DEFAULT_RATIO,
                leaf(0),
                leaf(1),
            ),
            Preset::TwoRows => split(SplitOrientation::Vertical, DEFAULT_RATIO, leaf(0), leaf(1)),
            Preset::ThreeColumns => split(
                SplitOrientation::Horizontal,
                1.0 / 3.0,
                leaf(0),
                split(
                    SplitOrientation::Horizontal,
                    DEFAULT_RATIO,
                    leaf(1),
                    leaf(2),
                ),
            ),
            Preset::Grid2x2 => split(
                SplitOrientation::Horizontal,
                DEFAULT_RATIO,
                split(SplitOrientation::Vertical, DEFAULT_RATIO, leaf(0), leaf(1)),
                split(SplitOrientation::Vertical, DEFAULT_RATIO, leaf(2), leaf(3)),
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaneTree {
    Leaf(PaneId),
    Split {
        orientation: SplitOrientation,
        ratio: f64,
        first: Box<PaneTree>,
        second: Box<PaneTree>,
    },
}

impl PaneTree {
    pub fn leaf(id: PaneId) -> Self {
        Self::Leaf(id)
    }

    pub fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            PaneTree::Leaf(id) => out.push(*id),
            PaneTree::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.leaves().len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn rect_of(&self, pane: PaneId) -> Option<(f64, f64, f64, f64)> {
        let mut rects = Vec::new();
        collect_rects(
            self,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            &mut rects,
        );
        rects
            .into_iter()
            .find(|(id, _)| *id == pane)
            .map(|(_, r)| (r.x, r.y, r.w, r.h))
    }

    /// Máximo de paneles que comparten una fila.
    pub fn max_columns(&self) -> usize {
        match self {
            PaneTree::Leaf(_) => 1,
            PaneTree::Split {
                orientation,
                first,
                second,
                ..
            } => match orientation {
                SplitOrientation::Horizontal => first.max_columns() + second.max_columns(),
                SplitOrientation::Vertical => first.max_columns().max(second.max_columns()),
            },
        }
    }

    pub fn normalize_ratios(&mut self) {
        match self {
            PaneTree::Leaf(_) => {}
            PaneTree::Split {
                ratio,
                first,
                second,
                ..
            } => {
                if !ratio.is_finite() {
                    *ratio = DEFAULT_RATIO;
                }
                *ratio = ratio.clamp(MIN_RATIO, MAX_RATIO);
                first.normalize_ratios();
                second.normalize_ratios();
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            PaneTree::Leaf(leaf) => *leaf == id,
            PaneTree::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    pub fn split(
        &mut self,
        target: PaneId,
        orientation: SplitOrientation,
        new_pane: PaneId,
    ) -> Result<(), SplitError> {
        if self.len() >= MAX_PANES {
            return Err(SplitError::MaxPanes);
        }
        if !self.contains(target) {
            return Err(SplitError::UnknownPane);
        }
        if split_in(self, target, orientation, new_pane) {
            Ok(())
        } else {
            Err(SplitError::UnknownPane)
        }
    }

    pub fn remove_leaf(&mut self, target: PaneId) -> Result<(), SplitError> {
        if self.len() <= 1 {
            return Err(SplitError::LastPane);
        }
        if !self.contains(target) {
            return Err(SplitError::UnknownPane);
        }
        if remove_in(self, target) {
            Ok(())
        } else {
            Err(SplitError::UnknownPane)
        }
    }

    pub fn path_of(&self, target: PaneId) -> Option<Vec<bool>> {
        let mut path = Vec::new();
        path_in(self, target, &mut path).map(|_| path)
    }

    pub fn ratio_at_path(&self, path: &[bool]) -> Option<f64> {
        match node_at_path(self, path) {
            Some(PaneTree::Split { ratio, .. }) => Some(*ratio),
            _ => None,
        }
    }

    pub fn set_ratio_at_path(&mut self, path: &[bool], ratio: f64) -> bool {
        match node_at_path_mut(self, path) {
            Some(PaneTree::Split { ratio: current, .. }) => {
                *current = ratio.clamp(MIN_RATIO, MAX_RATIO);
                true
            }
            _ => false,
        }
    }

    /// Panel vecino en una dirección, usando la geometría de los ratios.
    pub fn neighbor(&self, active: PaneId, direction: Direction) -> Option<PaneId> {
        let mut rects = Vec::new();
        collect_rects(
            self,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            &mut rects,
        );
        let a = rects.iter().find(|(id, _)| *id == active)?.1;
        let mut best: Option<(PaneId, f64, f64)> = None;
        for (id, r) in &rects {
            if *id == active {
                continue;
            }
            let overlap = match direction {
                Direction::Left | Direction::Right => {
                    r.y < a.y + a.h - f64::EPSILON && r.y + r.h > a.y + f64::EPSILON
                }
                Direction::Up | Direction::Down => {
                    r.x < a.x + a.w - f64::EPSILON && r.x + r.w > a.x + f64::EPSILON
                }
            };
            if !overlap {
                continue;
            }
            let in_direction = match direction {
                Direction::Left => r.x + r.w <= a.x + f64::EPSILON,
                Direction::Right => r.x >= a.x + a.w - f64::EPSILON,
                Direction::Up => r.y + r.h <= a.y + f64::EPSILON,
                Direction::Down => r.y >= a.y + a.h - f64::EPSILON,
            };
            if !in_direction {
                continue;
            }
            let gap = match direction {
                Direction::Left => a.x - (r.x + r.w),
                Direction::Right => r.x - (a.x + a.w),
                Direction::Up => a.y - (r.y + r.h),
                Direction::Down => r.y - (a.y + a.h),
            };
            let center = ((a.x + a.w / 2.0) - (r.x + r.w / 2.0)).abs()
                + ((a.y + a.h / 2.0) - (r.y + r.h / 2.0)).abs();
            let better = best
                .map(|(_, bg, bc)| (gap, center) < (bg, bc))
                .unwrap_or(true);
            if better {
                best = Some((*id, gap, center));
            }
        }
        best.map(|(id, _, _)| id)
    }
}

fn split_in(
    tree: &mut PaneTree,
    target: PaneId,
    orientation: SplitOrientation,
    new_pane: PaneId,
) -> bool {
    if let PaneTree::Leaf(id) = tree {
        if *id == target {
            let sibling = *id;
            *tree = PaneTree::Split {
                orientation,
                ratio: DEFAULT_RATIO,
                first: Box::new(PaneTree::Leaf(sibling)),
                second: Box::new(PaneTree::Leaf(new_pane)),
            };
            return true;
        }
        return false;
    }
    if let PaneTree::Split { first, second, .. } = tree {
        return split_in(first, target, orientation, new_pane)
            || split_in(second, target, orientation, new_pane);
    }
    false
}

fn remove_in(tree: &mut PaneTree, target: PaneId) -> bool {
    let PaneTree::Split { first, second, .. } = tree else {
        return false;
    };
    if matches!(**first, PaneTree::Leaf(id) if id == target) {
        let sibling = std::mem::replace(second.as_mut(), PaneTree::Leaf(0));
        *tree = sibling;
        return true;
    }
    if matches!(**second, PaneTree::Leaf(id) if id == target) {
        let sibling = std::mem::replace(first.as_mut(), PaneTree::Leaf(0));
        *tree = sibling;
        return true;
    }
    remove_in(first, target) || remove_in(second, target)
}

fn path_in(tree: &PaneTree, target: PaneId, path: &mut Vec<bool>) -> Option<()> {
    match tree {
        PaneTree::Leaf(id) if *id == target => Some(()),
        PaneTree::Leaf(_) => None,
        PaneTree::Split { first, second, .. } => {
            path.push(false);
            if path_in(first, target, path).is_some() {
                return Some(());
            }
            path.pop();
            path.push(true);
            if path_in(second, target, path).is_some() {
                return Some(());
            }
            path.pop();
            None
        }
    }
}

fn node_at_path<'a>(tree: &'a PaneTree, path: &[bool]) -> Option<&'a PaneTree> {
    let mut node = tree;
    for right in path {
        match node {
            PaneTree::Split { first, second, .. } => {
                node = if *right { second } else { first };
            }
            PaneTree::Leaf(_) => return None,
        }
    }
    Some(node)
}

fn node_at_path_mut<'a>(tree: &'a mut PaneTree, path: &[bool]) -> Option<&'a mut PaneTree> {
    let mut node = tree;
    for right in path {
        match node {
            PaneTree::Split { first, second, .. } => {
                node = if *right { second } else { first };
            }
            PaneTree::Leaf(_) => return None,
        }
    }
    Some(node)
}

#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

fn collect_rects(tree: &PaneTree, rect: Rect, out: &mut Vec<(PaneId, Rect)>) {
    match tree {
        PaneTree::Leaf(id) => out.push((*id, rect)),
        PaneTree::Split {
            orientation,
            ratio,
            first,
            second,
        } => match orientation {
            SplitOrientation::Horizontal => {
                let w = rect.w * ratio;
                collect_rects(
                    first,
                    Rect {
                        x: rect.x,
                        y: rect.y,
                        w,
                        h: rect.h,
                    },
                    out,
                );
                collect_rects(
                    second,
                    Rect {
                        x: rect.x + w,
                        y: rect.y,
                        w: rect.w - w,
                        h: rect.h,
                    },
                    out,
                );
            }
            SplitOrientation::Vertical => {
                let h = rect.h * ratio;
                collect_rects(
                    first,
                    Rect {
                        x: rect.x,
                        y: rect.y,
                        w: rect.w,
                        h,
                    },
                    out,
                );
                collect_rects(
                    second,
                    Rect {
                        x: rect.x,
                        y: rect.y + h,
                        w: rect.w,
                        h: rect.h - h,
                    },
                    out,
                );
            }
        },
    }
}

fn remap(tree: &PaneTree, map: &HashMap<PaneId, PaneId>) -> PaneTree {
    match tree {
        PaneTree::Leaf(id) => PaneTree::Leaf(map.get(id).copied().unwrap_or(*id)),
        PaneTree::Split {
            orientation,
            ratio,
            first,
            second,
        } => PaneTree::Split {
            orientation: *orientation,
            ratio: *ratio,
            first: Box::new(remap(first, map)),
            second: Box::new(remap(second, map)),
        },
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
    pub fn restore(tabs: Vec<RestoreTab>, active: usize) -> Result<Self, RestoreError> {
        if tabs.is_empty() {
            return Err(RestoreError::Empty);
        }
        if active >= tabs.len() {
            return Err(RestoreError::InvalidActiveTab);
        }
        let mut seen = std::collections::HashSet::new();
        let mut restored = Vec::with_capacity(tabs.len());
        let mut next_pane = 1;
        for (index, tab) in tabs.into_iter().enumerate() {
            let leaves = tab.layout.leaves();
            if leaves.len() > MAX_PANES {
                return Err(RestoreError::TooManyPanes);
            }
            if !leaves.contains(&tab.active_pane) {
                return Err(RestoreError::InvalidActivePane);
            }
            for leaf in &leaves {
                if !seen.insert(*leaf) {
                    return Err(RestoreError::DuplicatePane);
                }
                next_pane = next_pane.max(*leaf + 1);
            }
            let mut layout = tab.layout;
            layout.normalize_ratios();
            restored.push(Tab {
                id: index as u64 + 1,
                layout,
                active_pane: tab.active_pane,
                title_override: tab.title_override,
                last_osc_title: tab.last_osc_title,
                cwd: tab.cwd,
                theme_id: tab.theme_id,
                activity: Activity::Idle,
            });
        }
        let next_tab = restored.len() as u64 + 1;
        let active = restored[active].id;
        Ok(Self {
            tabs: restored,
            active,
            next_tab,
            next_pane,
        })
    }

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
        let mut map = HashMap::new();
        for pane in source.layout.leaves() {
            let new_pane = self.next_pane;
            self.next_pane += 1;
            map.insert(pane, new_pane);
        }
        let layout = remap(&source.layout, &map);
        let active_pane = map.get(&source.active_pane).copied()?;
        let new_id = self.next_tab;
        self.next_tab += 1;
        self.tabs.push(Tab {
            id: new_id,
            layout,
            active_pane,
            title_override: source.title_override,
            last_osc_title: source.last_osc_title,
            cwd: source.cwd,
            theme_id: source.theme_id,
            activity: source.activity,
        });
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

    /// Fija el tema de la pestaña (`None` = seguir el tema global).
    pub fn set_theme_id(&mut self, id: TabId, theme_id: Option<String>) -> bool {
        match self.tabs.iter_mut().find(|tab| tab.id == id) {
            Some(tab) => {
                tab.theme_id = theme_id;
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

    pub fn split_pane(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
        orientation: SplitOrientation,
    ) -> Result<PaneId, SplitError> {
        let tab = self.get(tab_id).ok_or(SplitError::UnknownPane)?;
        if tab.layout.len() >= MAX_PANES {
            return Err(SplitError::MaxPanes);
        }
        if !tab.layout.contains(pane_id) {
            return Err(SplitError::UnknownPane);
        }
        let new_pane = self.next_pane;
        self.next_pane += 1;
        let tab = self.get_mut(tab_id).ok_or(SplitError::UnknownPane)?;
        tab.layout
            .split(pane_id, orientation, new_pane)
            .map_err(|_| SplitError::UnknownPane)?;
        tab.active_pane = new_pane;
        Ok(new_pane)
    }

    /// Cierra un panel. Devuelve false si era el último (la pestaña se cierra
    /// fuera del modelo).
    pub fn close_pane(&mut self, tab_id: TabId, pane_id: PaneId) -> bool {
        let Some(tab) = self.get_mut(tab_id) else {
            return false;
        };
        if !tab.layout.contains(pane_id) || tab.layout.len() <= 1 {
            return false;
        }
        if tab.layout.remove_leaf(pane_id).is_err() {
            return false;
        }
        if tab.active_pane == pane_id {
            tab.active_pane = tab.layout.leaves()[0];
        }
        true
    }

    pub fn focus_pane(&mut self, tab_id: TabId, pane_id: PaneId) -> bool {
        match self.get_mut(tab_id) {
            Some(tab) if tab.layout.contains(pane_id) => {
                tab.active_pane = pane_id;
                true
            }
            _ => false,
        }
    }

    pub fn focus_direction(&mut self, tab_id: TabId, direction: Direction) -> bool {
        let Some(tab) = self.get_mut(tab_id) else {
            return false;
        };
        match tab.layout.neighbor(tab.active_pane, direction) {
            Some(next) => {
                tab.active_pane = next;
                true
            }
            None => false,
        }
    }

    /// Aplica un preset al panel activo. Solo agrega paneles; si la pestaña
    /// ya tiene más que los del preset, se rechaza para no matar procesos.
    pub fn apply_preset(
        &mut self,
        tab_id: TabId,
        preset: Preset,
    ) -> Result<Vec<PaneId>, SplitError> {
        let (mut ids, active) = {
            let tab = self.get(tab_id).ok_or(SplitError::UnknownPane)?;
            (tab.layout.leaves(), tab.active_pane)
        };
        let target = preset.panes();
        if ids.len() > target {
            return Err(SplitError::TooManyPanes);
        }
        let mut created = Vec::new();
        while ids.len() < target {
            let id = self.next_pane;
            self.next_pane += 1;
            ids.push(id);
            created.push(id);
        }
        let tab = self.get_mut(tab_id).ok_or(SplitError::UnknownPane)?;
        tab.layout = preset.build(&ids);
        if !ids.contains(&active) {
            tab.active_pane = ids[0];
        }
        Ok(created)
    }

    /// Ajusta el split más cercano del panel activo en el eje de la dirección.
    pub fn nudge_split(&mut self, tab_id: TabId, direction: Direction, delta: f64) -> bool {
        let Some(tab) = self.get_mut(tab_id) else {
            return false;
        };
        let Some(path) = tab.layout.path_of(tab.active_pane) else {
            return false;
        };
        let wanted = match direction {
            Direction::Left | Direction::Right => SplitOrientation::Horizontal,
            Direction::Up | Direction::Down => SplitOrientation::Vertical,
        };
        for depth in (0..path.len()).rev() {
            let prefix = &path[..depth];
            let Some(PaneTree::Split {
                orientation, ratio, ..
            }) = node_at_path(&tab.layout, prefix)
            else {
                continue;
            };
            if *orientation != wanted {
                continue;
            }
            // El atajo mueve el divisor en la dirección de la flecha.
            let increase = matches!(direction, Direction::Right | Direction::Down);
            let next = *ratio + if increase { delta } else { -delta };
            return tab.layout.set_ratio_at_path(prefix, next);
        }
        false
    }

    pub fn set_ratio(&mut self, tab_id: TabId, path: &[bool], ratio: f64) -> bool {
        match self.get_mut(tab_id) {
            Some(tab) => tab.layout.set_ratio_at_path(path, ratio),
            None => false,
        }
    }

    pub fn ratio(&self, tab_id: TabId, path: &[bool]) -> Option<f64> {
        self.get(tab_id)?.layout.ratio_at_path(path)
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
    fn duplicate_copies_context_and_remaps_panes() {
        let mut m = TabModel::new("/a");
        let id = m.active_id();
        let first = m.active().active_pane;
        let second = m
            .split_pane(id, first, SplitOrientation::Horizontal)
            .unwrap();
        m.rename(id, Some("x".into()));
        let dup = m.duplicate(id).unwrap();
        assert_eq!(m.active_id(), dup);
        assert_eq!(m.get(dup).unwrap().cwd, PathBuf::from("/a"));
        assert_eq!(m.get(dup).unwrap().title_override.as_deref(), Some("x"));
        let panes = m.pane_ids(dup).unwrap();
        assert_eq!(panes.len(), 2);
        assert!(!panes.contains(&first) && !panes.contains(&second));
        assert!(panes.contains(&m.get(dup).unwrap().active_pane));
    }

    #[test]
    fn pane_ids_and_containment() {
        let m = TabModel::new("/a");
        let panes = m.pane_ids(m.active_id()).unwrap();
        assert_eq!(panes.len(), 1);
        assert!(m.contains_pane(panes[0]));
        assert!(!m.contains_pane(999));
    }

    #[test]
    fn geometry_columns_and_min_width() {
        let grid = Preset::Grid2x2.build(&[1, 2, 3, 4]);
        assert_eq!(grid.max_columns(), 2);
        assert_eq!(grid.rect_of(1), Some((0.0, 0.0, 0.5, 0.5)));
        assert_eq!(grid.rect_of(2), Some((0.0, 0.5, 0.5, 0.5)));
        assert_eq!(grid.rect_of(3), Some((0.5, 0.0, 0.5, 0.5)));
        assert_eq!(grid.rect_of(4), Some((0.5, 0.5, 0.5, 0.5)));
        let three = Preset::ThreeColumns.build(&[1, 2, 3]);
        assert_eq!(three.max_columns(), 3);
        let rows = Preset::TwoRows.build(&[1, 2]);
        assert_eq!(rows.max_columns(), 1);
        assert_eq!(Preset::TwoColumns.columns(), 2);
        assert_eq!(Preset::ThreeColumns.columns(), 3);
        assert_eq!(Preset::TwoRows.columns(), 1);
        assert_eq!(Preset::Grid2x2.columns(), 2);
    }

    #[test]
    fn min_width_policy() {
        assert!(fits_min_width(1024.0, 2));
        assert!(fits_min_width(1024.0, 3));
        assert!(!fits_min_width(900.0, 3));
        assert!(!fits_min_width(600.0, 2));
        assert!(fits_min_width(0.0, 4));
        assert!(fits_min_width(1024.0, 0));
    }

    fn restore_tab(layout: PaneTree, active_pane: PaneId, cwd: &str) -> RestoreTab {
        RestoreTab {
            layout,
            active_pane,
            cwd: PathBuf::from(cwd),
            title_override: None,
            last_osc_title: None,
            theme_id: None,
        }
    }

    #[test]
    fn restore_preserves_tree_ratios_and_focus() {
        let layout = PaneTree::Split {
            orientation: SplitOrientation::Horizontal,
            ratio: 0.3,
            first: Box::new(PaneTree::Leaf(10)),
            second: Box::new(PaneTree::Split {
                orientation: SplitOrientation::Vertical,
                ratio: 0.6,
                first: Box::new(PaneTree::Leaf(11)),
                second: Box::new(PaneTree::Leaf(12)),
            }),
        };
        let mut second_tab = restore_tab(PaneTree::Leaf(20), 20, "/var");
        second_tab.title_override = Some("logs".into());
        let model =
            TabModel::restore(vec![restore_tab(layout, 12, "/tmp"), second_tab], 1).unwrap();
        assert_eq!(model.len(), 2);
        assert_eq!(model.active_id(), 2);
        assert_eq!(model.active().title_override.as_deref(), Some("logs"));
        let first = model.get(1).unwrap();
        assert_eq!(first.active_pane, 12);
        assert_eq!(first.layout.ratio_at_path(&[]), Some(0.3));
        assert_eq!(first.layout.ratio_at_path(&[true]), Some(0.6));
        assert_eq!(model.pane_ids(1).unwrap(), vec![10, 11, 12]);
        let mut model = model;
        let new_pane = model.split_pane(1, 10, SplitOrientation::Vertical).unwrap();
        assert!(new_pane > 12);
    }

    #[test]
    fn restore_rejects_invalid_sessions() {
        assert_eq!(
            TabModel::restore(Vec::new(), 0).err(),
            Some(RestoreError::Empty)
        );
        let tab = restore_tab(PaneTree::Leaf(1), 1, "/tmp");
        assert_eq!(
            TabModel::restore(vec![tab.clone()], 3).err(),
            Some(RestoreError::InvalidActiveTab)
        );
        let mut invalid = tab.clone();
        invalid.active_pane = 9;
        assert_eq!(
            TabModel::restore(vec![invalid], 0).err(),
            Some(RestoreError::InvalidActivePane)
        );
        let grid = PaneTree::Split {
            orientation: SplitOrientation::Horizontal,
            ratio: 0.5,
            first: Box::new(PaneTree::Leaf(1)),
            second: Box::new(PaneTree::Split {
                orientation: SplitOrientation::Vertical,
                ratio: 0.5,
                first: Box::new(PaneTree::Split {
                    orientation: SplitOrientation::Horizontal,
                    ratio: 0.5,
                    first: Box::new(PaneTree::Leaf(2)),
                    second: Box::new(PaneTree::Leaf(3)),
                }),
                second: Box::new(PaneTree::Leaf(4)),
            }),
        };
        let five = PaneTree::Split {
            orientation: SplitOrientation::Horizontal,
            ratio: 0.5,
            first: Box::new(grid),
            second: Box::new(PaneTree::Leaf(5)),
        };
        assert_eq!(
            TabModel::restore(vec![restore_tab(five, 1, "/tmp")], 0).err(),
            Some(RestoreError::TooManyPanes)
        );
        assert_eq!(
            TabModel::restore(
                vec![
                    restore_tab(PaneTree::Leaf(1), 1, "/tmp"),
                    restore_tab(PaneTree::Leaf(1), 1, "/var")
                ],
                0
            )
            .err(),
            Some(RestoreError::DuplicatePane)
        );
    }

    #[test]
    fn restore_clamps_ratios() {
        let layout = PaneTree::Split {
            orientation: SplitOrientation::Horizontal,
            ratio: 5.0,
            first: Box::new(PaneTree::Leaf(1)),
            second: Box::new(PaneTree::Leaf(2)),
        };
        let model = TabModel::restore(vec![restore_tab(layout, 1, "/tmp")], 0).unwrap();
        assert_eq!(model.active().layout.ratio_at_path(&[]), Some(0.9));
    }

    #[test]
    fn presets_build_the_expected_shapes() {
        let two = Preset::TwoColumns.build(&[1, 2]);
        assert_eq!(two.leaves(), vec![1, 2]);
        match two {
            PaneTree::Split {
                orientation, ratio, ..
            } => {
                assert_eq!(orientation, SplitOrientation::Horizontal);
                assert_eq!(ratio, DEFAULT_RATIO);
            }
            _ => panic!("expected split"),
        }
        let three = Preset::ThreeColumns.build(&[1, 2, 3]);
        assert_eq!(three.leaves(), vec![1, 2, 3]);
        assert!((three.ratio_at_path(&[]).unwrap() - 1.0 / 3.0).abs() < 1e-9);
        let grid = Preset::Grid2x2.build(&[1, 2, 3, 4]);
        assert_eq!(grid.leaves(), vec![1, 2, 3, 4]);
        assert_eq!(grid.len(), 4);
    }

    #[test]
    fn apply_preset_adds_panes_and_keeps_active() {
        let mut m = TabModel::new("/a");
        let tab = m.active_id();
        let first = m.active().active_pane;
        let created = m.apply_preset(tab, Preset::TwoColumns).unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(m.pane_ids(tab).unwrap().len(), 2);
        assert_eq!(m.active().active_pane, first);
        let created = m.apply_preset(tab, Preset::Grid2x2).unwrap();
        assert_eq!(created.len(), 2);
        assert_eq!(m.pane_ids(tab).unwrap().len(), 4);
        assert!(m.pane_ids(tab).unwrap()[..2].contains(&first));
        assert_eq!(
            m.apply_preset(tab, Preset::TwoColumns),
            Err(SplitError::TooManyPanes)
        );
    }

    #[test]
    fn split_adds_pane_and_activates_it() {
        let mut m = TabModel::new("/a");
        let tab = m.active_id();
        let first = m.active().active_pane;
        let second = m
            .split_pane(tab, first, SplitOrientation::Horizontal)
            .unwrap();
        assert_eq!(m.active().active_pane, second);
        assert_eq!(m.pane_ids(tab).unwrap(), vec![first, second]);
        let layout = &m.active().layout;
        match layout {
            PaneTree::Split {
                orientation,
                ratio,
                first: f,
                second: s,
            } => {
                assert_eq!(*orientation, SplitOrientation::Horizontal);
                assert_eq!(*ratio, DEFAULT_RATIO);
                assert_eq!(**f, PaneTree::Leaf(first));
                assert_eq!(**s, PaneTree::Leaf(second));
            }
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn split_rejects_fifth_pane_and_unknown_pane() {
        let mut m = TabModel::new("/a");
        let tab = m.active_id();
        let mut active = m.active().active_pane;
        for _ in 0..3 {
            active = m
                .split_pane(tab, active, SplitOrientation::Vertical)
                .unwrap();
        }
        assert_eq!(m.pane_ids(tab).unwrap().len(), MAX_PANES);
        assert_eq!(
            m.split_pane(tab, active, SplitOrientation::Horizontal),
            Err(SplitError::MaxPanes)
        );
        let other = m.new_tab("/b");
        assert_eq!(
            m.split_pane(other, active, SplitOrientation::Horizontal),
            Err(SplitError::UnknownPane)
        );
    }

    #[test]
    fn close_pane_promotes_sibling_and_rejects_last() {
        let mut m = TabModel::new("/a");
        let tab = m.active_id();
        let first = m.active().active_pane;
        let second = m
            .split_pane(tab, first, SplitOrientation::Horizontal)
            .unwrap();
        assert_eq!(m.active().active_pane, second);
        assert!(m.close_pane(tab, second));
        assert_eq!(m.pane_ids(tab).unwrap(), vec![first]);
        assert_eq!(m.active().active_pane, first);
        assert!(!m.close_pane(tab, first));
    }

    #[test]
    fn removes_deep_leaf_and_keeps_tree_valid() {
        let mut m = TabModel::new("/a");
        let tab = m.active_id();
        let p1 = m.active().active_pane;
        let p2 = m.split_pane(tab, p1, SplitOrientation::Horizontal).unwrap();
        m.focus_pane(tab, p1);
        let p3 = m.split_pane(tab, p1, SplitOrientation::Vertical).unwrap();
        assert_eq!(m.pane_ids(tab).unwrap(), vec![p1, p3, p2]);
        assert!(m.close_pane(tab, p3));
        assert_eq!(m.pane_ids(tab).unwrap(), vec![p1, p2]);
        match &m.active().layout {
            PaneTree::Split { first, second, .. } => {
                assert_eq!(**first, PaneTree::Leaf(p1));
                assert_eq!(**second, PaneTree::Leaf(p2));
            }
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn focus_direction_on_2x2_grid() {
        let mut m = TabModel::new("/a");
        let tab = m.active_id();
        let tl = m.active().active_pane;
        let tr = m.split_pane(tab, tl, SplitOrientation::Horizontal).unwrap();
        m.focus_pane(tab, tl);
        let bl = m.split_pane(tab, tl, SplitOrientation::Vertical).unwrap();
        m.focus_pane(tab, tr);
        let br = m.split_pane(tab, tr, SplitOrientation::Vertical).unwrap();

        m.focus_pane(tab, tl);
        assert!(m.focus_direction(tab, Direction::Right));
        assert_eq!(m.active().active_pane, tr);
        m.focus_pane(tab, tl);
        assert!(m.focus_direction(tab, Direction::Down));
        assert_eq!(m.active().active_pane, bl);
        m.focus_pane(tab, bl);
        assert!(m.focus_direction(tab, Direction::Right));
        assert_eq!(m.active().active_pane, br);
        m.focus_pane(tab, br);
        assert!(m.focus_direction(tab, Direction::Up));
        assert_eq!(m.active().active_pane, tr);
        m.focus_pane(tab, tr);
        assert!(m.focus_direction(tab, Direction::Left));
        assert_eq!(m.active().active_pane, tl);
        m.focus_pane(tab, tl);
        assert!(!m.focus_direction(tab, Direction::Left));
        assert!(!m.focus_direction(tab, Direction::Up));
    }

    #[test]
    fn nudge_adjusts_the_nearest_matching_split() {
        let mut m = TabModel::new("/a");
        let tab = m.active_id();
        let first = m.active().active_pane;
        let second = m
            .split_pane(tab, first, SplitOrientation::Horizontal)
            .unwrap();

        m.focus_pane(tab, first);
        assert!(m.nudge_split(tab, Direction::Right, 0.05));
        assert!((m.ratio(tab, &[]).unwrap() - 0.55).abs() < 1e-9);
        assert!(m.nudge_split(tab, Direction::Left, 0.05));
        assert!((m.ratio(tab, &[]).unwrap() - 0.50).abs() < 1e-9);

        m.focus_pane(tab, second);
        assert!(m.nudge_split(tab, Direction::Right, 0.05));
        assert!((m.ratio(tab, &[]).unwrap() - 0.55).abs() < 1e-9);
        assert!(!m.nudge_split(tab, Direction::Up, 0.05));

        let down = m
            .split_pane(tab, second, SplitOrientation::Vertical)
            .unwrap();
        m.focus_pane(tab, down);
        assert!(m.nudge_split(tab, Direction::Down, 0.05));
        let down_path = m.active().layout.path_of(down).unwrap();
        let split_path = &down_path[..down_path.len() - 1];
        assert!((m.ratio(tab, split_path).unwrap() - 0.55).abs() < 1e-9);
    }

    #[test]
    fn ratios_are_clamped_and_addressed_by_path() {
        let mut m = TabModel::new("/a");
        let tab = m.active_id();
        let first = m.active().active_pane;
        let second = m
            .split_pane(tab, first, SplitOrientation::Horizontal)
            .unwrap();
        assert_eq!(m.active().layout.path_of(second), Some(vec![true]));
        assert!(m.set_ratio(tab, &[], 0.3));
        assert_eq!(m.ratio(tab, &[]), Some(0.3));
        assert!(m.set_ratio(tab, &[], 0.0));
        assert_eq!(m.ratio(tab, &[]), Some(MIN_RATIO));
        assert!(m.set_ratio(tab, &[], 1.0));
        assert_eq!(m.ratio(tab, &[]), Some(MAX_RATIO));
        assert!(!m.set_ratio(tab, &[true], 0.5));
    }
}
