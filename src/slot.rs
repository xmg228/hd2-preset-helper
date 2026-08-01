use crate::item::ItemKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    Stratagem,
    StratagemEmpty,
    Booster,
    /// Special "no booster" list cell; occupies the grid but has no item template.
    NoBoosterOption,
    /// Filled booster slot on the loadout home screen.
    HomeBooster,
    /// Empty booster slot on the loadout home screen.
    HomeBoosterEmpty,
}

impl SlotKind {
    pub const fn classification_kind(self) -> Option<ItemKind> {
        match self {
            Self::Stratagem => Some(ItemKind::Stratagem),
            Self::Booster | Self::HomeBooster => Some(ItemKind::Booster),
            Self::StratagemEmpty | Self::NoBoosterOption | Self::HomeBoosterEmpty => None,
        }
    }

    pub const fn is_selectable_item_for(self, item_kind: ItemKind) -> bool {
        matches!(
            (item_kind, self),
            (ItemKind::Stratagem, Self::Stratagem)
                | (ItemKind::Booster, Self::Booster)
        )
    }

    pub const fn is_home_booster(self) -> bool {
        matches!(self, Self::HomeBooster | Self::HomeBoosterEmpty)
    }
}

/// Page-level layout expected by the detector and attached to each observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotLayout {
    Home,
    List(ItemKind),
}

impl SlotLayout {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::List(ItemKind::Stratagem) => "stratagem_list",
            Self::List(ItemKind::Booster) => "booster_list",
        }
    }
}
