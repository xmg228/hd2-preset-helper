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
