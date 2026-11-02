use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Stratagem,
    Booster,
}

impl ItemKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stratagem => "stratagem",
            Self::Booster => "booster",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StratagemCategory {
    Offensive,
    Supply,
    Defensive,
}

impl StratagemCategory {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Offensive => "offensive",
            Self::Supply => "supply",
            Self::Defensive => "defensive",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EquipmentKind {
    Helmet,
    Armor,
    Cape,
    Primary,
    Secondary,
    Throwable,
}

impl EquipmentKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Helmet => "helmet",
            Self::Armor => "armor",
            Self::Cape => "cape",
            Self::Primary => "primary",
            Self::Secondary => "secondary",
            Self::Throwable => "throwable",
        }
    }

    pub const fn has_categories(self) -> bool {
        !matches!(self, Self::Helmet | Self::Cape)
    }

    pub const fn list_columns(self) -> usize {
        match self {
            Self::Primary | Self::Secondary => 2,
            _ => 3,
        }
    }

    pub const ALL: [Self; 6] = [
        Self::Helmet,
        Self::Armor,
        Self::Cape,
        Self::Primary,
        Self::Secondary,
        Self::Throwable,
    ];

    /// Index in the six-card entry, also used for in-memory equipment slots.
    pub fn index(self) -> usize {
        match self {
            Self::Helmet => 0,
            Self::Armor => 1,
            Self::Cape => 2,
            Self::Primary => 3,
            Self::Secondary => 4,
            Self::Throwable => 5,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "Vec<EquipmentKind>")]
pub struct EquipmentSelection([bool; 6]);

impl Default for EquipmentSelection {
    fn default() -> Self {
        Self([true; 6])
    }
}

impl From<Vec<EquipmentKind>> for EquipmentSelection {
    fn from(kinds: Vec<EquipmentKind>) -> Self {
        let mut flags = [false; 6];
        for kind in kinds {
            flags[kind.index()] = true;
        }
        Self(flags)
    }
}

impl std::fmt::Debug for EquipmentSelection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}

impl EquipmentSelection {
    pub fn flags(self) -> [bool; 6] {
        self.0
    }
    pub fn contains(self, kind: EquipmentKind) -> bool {
        self.0[kind.index()]
    }
    pub fn toggle(&mut self, kind: EquipmentKind) {
        self.0[kind.index()] = !self.0[kind.index()];
    }
    pub fn is_empty(self) -> bool {
        !self.0.into_iter().any(|selected| selected)
    }
    pub fn iter(self) -> impl Iterator<Item = EquipmentKind> {
        EquipmentKind::ALL
            .into_iter()
            .filter(move |&kind| self.contains(kind))
    }
}
