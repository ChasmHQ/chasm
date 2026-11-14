use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_PANES: usize = 64;
pub const MAX_OWNER_LEN: usize = 128;
pub const MIN_PANE_SIZE: f32 = 120.0;
pub const MAX_PANE_SIZE: f32 = 4096.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneKind {
    Navigation,
    Document,
    CallResult,
    Storage,
    DebugCode,
    DebugInspector,
    DebugInspection,
    DebugSession,
    Console,
    Logs,
    Jobs,
    Investigation,
}

impl PaneKind {
    fn key(self) -> &'static str {
        match self {
            Self::Navigation => "navigation",
            Self::Document => "document",
            Self::CallResult => "call_result",
            Self::Storage => "storage",
            Self::DebugCode => "debug_code",
            Self::DebugInspector => "debug_inspector",
            Self::DebugInspection => "debug_inspection",
            Self::DebugSession => "debug_session",
            Self::Console => "console",
            Self::Logs => "logs",
            Self::Jobs => "jobs",
            Self::Investigation => "investigation",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Document => "Document",
            Self::CallResult => "Call Result",
            Self::Storage => "Storage",
            // Short and distinct: these sit side by side in narrow pane headers.
            Self::DebugCode => "Code",
            Self::DebugInspector => "Inspector",
            Self::DebugInspection => "Low-level",
            Self::DebugSession => "Session",
            Self::Console => "Console",
            Self::Logs => "Logs",
            Self::Jobs => "Jobs",
            Self::Investigation => "Investigation",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneId {
    pub kind: PaneKind,
    /// A non-secret opaque document identifier, never a filename or path.
    pub owner: Option<String>,
}

impl PaneId {
    pub fn new(kind: PaneKind, owner: Option<String>) -> Self {
        Self { kind, owner }
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(owner) = &self.owner {
            if owner.is_empty()
                || owner.len() > MAX_OWNER_LEN
                || !owner.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            {
                return Err("Owner must be a non-empty opaque identifier of at most 128 ASCII letters, digits, hyphens or underscores".into());
            }
        }
        Ok(())
    }

    /// Keys are unique for valid IDs. Invalid owners are never included in a key.
    pub fn stable_key(&self) -> String {
        match &self.owner {
            Some(owner) if self.validate().is_ok() => format!("{}:{owner}", self.kind.key()),
            Some(_) => format!("{}:invalid", self.kind.key()),
            None => self.kind.key().to_owned(),
        }
    }

    pub fn title(&self) -> &'static str {
        self.kind.title()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Zone {
    Left,
    Center,
    Right,
    Bottom,
}

const ZONES: [Zone; 4] = [Zone::Left, Zone::Center, Zone::Right, Zone::Bottom];

/// x/y are physical outer-frame coordinates; width/height are logical client pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl Geometry {
    pub fn validate(&self) -> Result<(), String> {
        if !(-1_000_000..=1_000_000).contains(&self.x)
            || !(-1_000_000..=1_000_000).contains(&self.y)
            || !(240..=8192).contains(&self.width)
            || !(160..=8192).contains(&self.height)
        {
            return Err("Window geometry is outside the allowed bounds".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Placement {
    Docked { zone: Zone },
    Floating { geometry: Geometry, return_zone: Zone },
    Hidden { return_zone: Zone },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanePlacement {
    pub id: PaneId,
    pub placement: Placement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Contracts,
    Debugging,
    Investigation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub schema_version: u32,
    pub panes: Vec<PanePlacement>,
    pub active: BTreeMap<Zone, String>,
    /// Split sizes are logical pixels, not fractions of the viewport.
    pub right_width: f32,
    pub bottom_height: f32,
    pub left_width: f32,
    /// Stable key of the maximized docked pane, if any.
    pub maximized: Option<String>,
}

impl Layout {
    pub fn preset(preset: Preset) -> Self {
        let mut layout = Self {
            schema_version: SCHEMA_VERSION,
            panes: Vec::new(),
            active: BTreeMap::new(),
            right_width: 320.0,
            bottom_height: 140.0,
            left_width: 240.0,
            maximized: None,
        };
        let panes: &[(PaneKind, Zone)] = match preset {
            Preset::Contracts => &[
                (PaneKind::Navigation, Zone::Left),
                (PaneKind::Document, Zone::Center),
                (PaneKind::Console, Zone::Bottom),
                (PaneKind::Logs, Zone::Bottom),
                (PaneKind::Jobs, Zone::Bottom),
            ],
            Preset::Debugging => &[
                (PaneKind::Navigation, Zone::Left),
                (PaneKind::DebugCode, Zone::Center),
                (PaneKind::Document, Zone::Center),
                (PaneKind::DebugInspector, Zone::Right),
                (PaneKind::DebugInspection, Zone::Right),
                (PaneKind::DebugSession, Zone::Right),
                (PaneKind::Console, Zone::Bottom),
                (PaneKind::Logs, Zone::Bottom),
                (PaneKind::Jobs, Zone::Bottom),
            ],
            Preset::Investigation => &[
                (PaneKind::Navigation, Zone::Left),
                (PaneKind::Investigation, Zone::Center),
                (PaneKind::Document, Zone::Center),
                (PaneKind::Console, Zone::Bottom),
                (PaneKind::Logs, Zone::Bottom),
                (PaneKind::Jobs, Zone::Bottom),
            ],
        };
        for &(kind, zone) in panes {
            layout.panes.push(PanePlacement { id: PaneId::new(kind, None), placement: Placement::Docked { zone } });
        }
        layout.repair_active();
        if preset == Preset::Contracts {
            for kind in [PaneKind::Console, PaneKind::Logs, PaneKind::Jobs] {
                let _ = layout.hide(&PaneId::new(kind, None));
            }
        }
        layout
    }

    /// Adds a pane if absent, or restores a hidden pane to its remembered zone.
    pub fn ensure(&mut self, id: PaneId, zone: Zone) -> Result<(), String> {
        id.validate()?;
        if let Some(index) = self.panes.iter().position(|pane| pane.id == id) {
            if let Placement::Hidden { return_zone } = self.panes[index].placement {
                self.panes[index].placement = Placement::Docked { zone: return_zone };
                self.active.insert(return_zone, id.stable_key());
            }
        } else {
            if self.panes.len() >= MAX_PANES {
                return Err("Layout has too many panes".into());
            }
            self.panes.push(PanePlacement { id, placement: Placement::Docked { zone } });
        }
        self.repair_active();
        Ok(())
    }

    pub fn placement(&self, id: &PaneId) -> Option<&Placement> {
        self.panes.iter().find(|pane| &pane.id == id).map(|pane| &pane.placement)
    }

    fn index(&self, id: &PaneId) -> Result<usize, String> {
        id.validate()?;
        self.panes.iter().position(|pane| &pane.id == id).ok_or_else(|| "Pane does not exist".into())
    }

    pub fn dock(&mut self, id: &PaneId, zone: Zone) -> Result<(), String> {
        let index = self.index(id)?;
        self.panes[index].placement = Placement::Docked { zone };
        self.active.insert(zone, id.stable_key());
        self.repair_active();
        Ok(())
    }

    /// The native host must successfully open the window before calling this method.
    pub fn float(&mut self, id: &PaneId, geometry: Geometry) -> Result<(), String> {
        geometry.validate()?;
        let index = self.index(id)?;
        let return_zone = match self.panes[index].placement {
            Placement::Docked { zone } => zone,
            Placement::Floating { return_zone, .. } | Placement::Hidden { return_zone } => return_zone,
        };
        self.panes[index].placement = Placement::Floating { geometry, return_zone };
        self.repair_active();
        Ok(())
    }

    pub fn hide(&mut self, id: &PaneId) -> Result<(), String> {
        let index = self.index(id)?;
        let return_zone = match self.panes[index].placement {
            Placement::Docked { zone } => zone,
            Placement::Floating { return_zone, .. } | Placement::Hidden { return_zone } => return_zone,
        };
        self.panes[index].placement = Placement::Hidden { return_zone };
        self.repair_active();
        Ok(())
    }

    pub fn close_float(&mut self, id: &PaneId) -> Result<(), String> {
        let index = self.index(id)?;
        match self.panes[index].placement {
            Placement::Floating { return_zone, .. } => self.dock(id, return_zone),
            _ => Err("Pane is not floating".into()),
        }
    }

    pub fn select(&mut self, zone: Zone, id: &PaneId) -> Result<(), String> {
        let index = self.index(id)?;
        if self.panes[index].placement != (Placement::Docked { zone }) {
            return Err("Pane is not docked in that zone".into());
        }
        self.active.insert(zone, id.stable_key());
        self.repair_active();
        Ok(())
    }

    /// Move an existing docked pane to a zero-based tab index in the destination zone.
    pub fn reorder(&mut self, id: &PaneId, zone: Zone, index: usize) -> Result<(), String> {
        let source = self.index(id)?;
        if !matches!(self.panes[source].placement, Placement::Docked { .. }) {
            return Err("Only a docked pane can be reordered".into());
        }
        let count = self.panes.iter().filter(|pane| pane.placement == Placement::Docked { zone }).count();
        let already_in_zone = self.panes[source].placement == Placement::Docked { zone };
        let target_count = count + usize::from(!already_in_zone);
        if index >= target_count {
            return Err("Tab index is outside the destination zone".into());
        }
        let mut pane = self.panes.remove(source);
        pane.placement = Placement::Docked { zone };
        let target = self
            .panes
            .iter()
            .enumerate()
            .filter(|(_, pane)| pane.placement == Placement::Docked { zone })
            .map(|(index, _)| index)
            .nth(index)
            .unwrap_or(self.panes.len());
        self.panes.insert(target, pane);
        self.active.insert(zone, id.stable_key());
        self.repair_active();
        Ok(())
    }

    /// Remove a pane and repair selection references.
    pub fn remove(&mut self, id: &PaneId) -> Result<(), String> {
        let index = self.index(id)?;
        self.panes.remove(index);
        self.repair_active();
        Ok(())
    }

    /// Drop panes whose owner is no longer part of the current document set.
    pub fn retain_owners(&mut self, mut exists: impl FnMut(&str) -> bool) {
        self.panes.retain(|pane| pane.id.owner.as_deref().is_none_or(&mut exists));
        self.repair_active();
    }

    pub fn set_sizes(&mut self, left: f32, right: f32, bottom: f32) -> Result<(), String> {
        if [left, right, bottom].iter().any(|size| !size.is_finite() || !(MIN_PANE_SIZE..=MAX_PANE_SIZE).contains(size))
        {
            return Err("Split sizes must be finite logical pixels within bounds".into());
        }
        self.left_width = left;
        self.right_width = right;
        self.bottom_height = bottom;
        Ok(())
    }

    pub fn reset(&mut self, preset: Preset) {
        *self = Self::preset(preset);
    }

    fn repair_active(&mut self) {
        self.active.retain(|zone, key| {
            self.panes
                .iter()
                .any(|pane| pane.id.stable_key() == *key && pane.placement == Placement::Docked { zone: *zone })
        });
        for zone in ZONES {
            if !self.active.contains_key(&zone) {
                if let Some(pane) = self.panes.iter().find(|pane| pane.placement == Placement::Docked { zone }) {
                    self.active.insert(zone, pane.id.stable_key());
                }
            }
        }
        if self.maximized.as_ref().is_some_and(|key| {
            !self.panes.iter().any(|pane| match pane.placement {
                Placement::Docked { zone } => pane.id.stable_key() == *key && self.active.get(&zone) == Some(key),
                _ => false,
            })
        }) {
            self.maximized = None;
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err("Unsupported layout schema version".into());
        }
        if self.panes.len() > MAX_PANES {
            return Err("Layout has too many panes".into());
        }
        if [self.left_width, self.right_width, self.bottom_height]
            .iter()
            .any(|size| !size.is_finite() || !(MIN_PANE_SIZE..=MAX_PANE_SIZE).contains(size))
        {
            return Err("Split sizes must be finite logical pixels within bounds".into());
        }
        let mut keys = HashSet::new();
        for pane in &self.panes {
            pane.id.validate()?;
            if !keys.insert(pane.id.stable_key()) {
                return Err("Duplicate pane ID".into());
            }
            if let Placement::Floating { geometry, .. } = &pane.placement {
                geometry.validate()?;
            }
        }
        for (zone, key) in &self.active {
            if !self
                .panes
                .iter()
                .any(|pane| pane.id.stable_key() == *key && pane.placement == Placement::Docked { zone: *zone })
            {
                return Err("Active pane is not docked in its zone".into());
            }
        }
        if self.maximized.as_ref().is_some_and(|key| {
            !self.panes.iter().any(|pane| match pane.placement {
                Placement::Docked { zone } => pane.id.stable_key() == *key && self.active.get(&zone) == Some(key),
                _ => false,
            })
        }) {
            return Err("Maximized pane is not the active docked pane".into());
        }
        Ok(())
    }

    pub fn from_json(value: &Value) -> Result<Self, String> {
        let layout: Self = serde_json::from_value(value.clone()).map_err(|_| "Invalid layout data".to_owned())?;
        layout.validate()?;
        Ok(layout)
    }

    pub fn to_json(&self) -> Result<Value, String> {
        self.validate()?;
        serde_json::to_value(self).map_err(|_| "Unable to serialize layout".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id(kind: PaneKind) -> PaneId {
        PaneId::new(kind, None)
    }
    fn geometry() -> Geometry {
        Geometry { x: -1920, y: -80, width: 640, height: 480, maximized: false }
    }

    #[test]
    fn presets_and_owned_float_round_trip() {
        for preset in [Preset::Contracts, Preset::Debugging, Preset::Investigation] {
            let mut layout = Layout::preset(preset);
            layout.validate().unwrap();
            let owned = PaneId::new(PaneKind::CallResult, Some("document-123".into()));
            layout.ensure(owned.clone(), Zone::Right).unwrap();
            layout.float(&owned, geometry()).unwrap();
            let json = layout.to_json().unwrap();
            assert_eq!(Layout::from_json(&json).unwrap(), layout);
            layout.close_float(&owned).unwrap();
            assert_eq!(layout.placement(&owned), Some(&Placement::Docked { zone: Zone::Right }));
            assert_eq!(layout.active[&Zone::Right], owned.stable_key());
            assert_eq!(owned.owner.as_deref(), Some("document-123"));
            layout.dock(&owned, Zone::Left).unwrap();
            assert_eq!(layout.active[&Zone::Left], owned.stable_key());
            layout.validate().unwrap();
        }
    }

    #[test]
    fn geometry_bounds_and_transactional_mutations() {
        let mut layout = Layout::preset(Preset::Contracts);
        let document = id(PaneKind::Document);
        assert!(geometry().validate().is_ok());
        for bad in [
            Geometry { x: i32::MIN, ..geometry() },
            Geometry { x: 1_000_001, ..geometry() },
            Geometry { width: 239, ..geometry() },
            Geometry { height: 159, ..geometry() },
            Geometry { width: 8193, ..geometry() },
            Geometry { height: 8193, ..geometry() },
        ] {
            let before = layout.clone();
            assert!(layout.float(&document, bad).is_err());
            assert_eq!(layout, before);
        }
        for bad in [f32::NAN, f32::INFINITY, -1.0, 0.5, 4097.0] {
            let before = layout.clone();
            assert!(layout.set_sizes(240.0, bad, 220.0).is_err());
            assert_eq!(layout, before);
        }
    }

    #[test]
    fn selection_and_hidden_restore_repair() {
        let mut layout = Layout::preset(Preset::Debugging);
        let code = id(PaneKind::DebugCode);
        let doc = id(PaneKind::Document);
        layout.select(Zone::Center, &doc).unwrap();
        layout.hide(&doc).unwrap();
        assert_eq!(layout.active[&Zone::Center], code.stable_key());
        assert!(layout.select(Zone::Center, &doc).is_err());
        layout.ensure(doc.clone(), Zone::Right).unwrap();
        assert_eq!(layout.placement(&doc), Some(&Placement::Docked { zone: Zone::Center }));
        assert_eq!(layout.active[&Zone::Center], doc.stable_key());
        layout.float(&doc, geometry()).unwrap();
        assert_eq!(layout.active[&Zone::Center], code.stable_key());
        layout.close_float(&doc).unwrap();
        layout.validate().unwrap();
    }

    #[test]
    fn rejects_malformed_layouts_and_owners() {
        let layout = Layout::preset(Preset::Contracts);
        let value = layout.to_json().unwrap();
        let mut bad = value.clone();
        bad["schema_version"] = json!(2);
        assert!(Layout::from_json(&bad).is_err());
        let mut bad = value.clone();
        bad["unexpected"] = json!(true);
        assert!(Layout::from_json(&bad).is_err());
        let mut bad = value.clone();
        bad["panes"][0]["id"]["extra"] = json!(1);
        assert!(Layout::from_json(&bad).is_err());
        let mut bad = value.clone();
        bad["panes"][0]["placement"]["extra"] = json!(1);
        assert!(Layout::from_json(&bad).is_err());
        let mut bad = value.clone();
        bad["panes"].as_array_mut().unwrap().push(value["panes"][0].clone());
        assert!(Layout::from_json(&bad).is_err());
        let mut bad = value.clone();
        bad["active"]["right"] = json!("document");
        assert!(Layout::from_json(&bad).is_err());
        let mut bad = value.clone();
        bad["left_width"] = json!(0.5);
        assert!(Layout::from_json(&bad).is_err());
        let mut bad = value.clone();
        bad["panes"][0]["id"]["owner"] = json!("/private/file");
        assert!(Layout::from_json(&bad).is_err());
        let mut bad = value.clone();
        bad["panes"][0]["placement"] = json!({"mode":"floating", "return_zone":"left", "geometry":{"x":0,"y":0,"width":240,"height":160,"maximized":false,"extra":1}});
        assert!(Layout::from_json(&bad).is_err());
        let mut bad = value.clone();
        bad["maximized"] = json!("nonexistent");
        assert!(Layout::from_json(&bad).is_err());
        let mut bad = value.clone();
        bad["panes"][0]["id"]["kind"] = json!("unknown");
        assert!(Layout::from_json(&bad).is_err());
        let bad_id = PaneId::new(PaneKind::Storage, Some("/private/file".into()));
        assert!(!bad_id.stable_key().contains("/private/file"));
        assert!(PaneId::new(PaneKind::Storage, Some("a".repeat(129))).validate().is_err());
    }

    #[test]
    fn pane_limit_maximize_and_explicit_size_bounds() {
        let mut layout = Layout::preset(Preset::Contracts);
        let doc = id(PaneKind::Document);
        layout.maximized = Some(doc.stable_key());
        layout.float(&doc, geometry()).unwrap();
        assert_eq!(layout.maximized, None);
        layout.close_float(&doc).unwrap();
        layout.set_sizes(MIN_PANE_SIZE, MAX_PANE_SIZE, MIN_PANE_SIZE).unwrap();
        while layout.panes.len() < MAX_PANES {
            let owner = format!("document-{}", layout.panes.len());
            layout.ensure(PaneId::new(PaneKind::Storage, Some(owner)), Zone::Right).unwrap();
        }
        let before = layout.clone();
        assert!(layout.ensure(PaneId::new(PaneKind::Storage, Some("one-too-many".into())), Zone::Right).is_err());
        assert_eq!(layout, before);
        layout.validate().unwrap();
    }
}
