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

#[derive(Clone, Copy, Debug, Serialize)]
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

    #[cfg(feature = "diagnostics")]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "helmet" => Self::Helmet,
            "armor" => Self::Armor,
            "cape" => Self::Cape,
            "primary" => Self::Primary,
            "secondary" => Self::Secondary,
            "throwable" => Self::Throwable,
            _ => return None,
        })
    }
}
