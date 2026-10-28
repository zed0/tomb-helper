//! Everything a hotkey can be bound to. Movement actions carry a `distance` (default 100).

use serde::{Deserialize, Serialize};

fn default_distance() -> f32 {
    100.0
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub enum Action {
    ToggleActive {},
    StorePosition {},
    RestorePosition {},
    SkipCutscene {},
    ResetSkipCutsceneTracker {},
    Forward {
        #[serde(default = "default_distance")]
        distance: f32,
    },
    Backward {
        #[serde(default = "default_distance")]
        distance: f32,
    },
    Left {
        #[serde(default = "default_distance")]
        distance: f32,
    },
    Right {
        #[serde(default = "default_distance")]
        distance: f32,
    },
    Up {
        #[serde(default = "default_distance")]
        distance: f32,
    },
    Down {
        #[serde(default = "default_distance")]
        distance: f32,
    },
    ToggleUI {},
    // What the overlay shows
    ToggleLaraCollision {},
    ToggleCoarseCollision {},
    ToggleDetailedCollision {},
    ToggleInteractables {},
    ToggleTriggers {},
    ToggleNpcs {},
    // Everything to do with collision at once
    ToggleCollision {},
    // Everything the overlay draws in the world at once (its window stays)
    ToggleOverlay {},
    // The overlay's settings window (what it draws stays)
    ToggleSettingsWindow {},
    // The developers' level select, from the main menu
    LevelSelect {},
}

impl Action {
    /// Whether holding its key does it again at the keyboard's repeat rate. Toggles don't: held, they'd flick on and
    /// off, and toggling the overlay faster than it can unload left it loaded but unhooked.
    pub fn repeats(&self) -> bool {
        !matches!(
            self,
            Action::ToggleActive {}
                | Action::ToggleUI {}
                | Action::ToggleLaraCollision {}
                | Action::ToggleCoarseCollision {}
                | Action::ToggleDetailedCollision {}
                | Action::ToggleInteractables {}
                | Action::ToggleTriggers {}
                | Action::ToggleNpcs {}
                | Action::ToggleCollision {}
                | Action::ToggleOverlay {}
                | Action::ToggleSettingsWindow {}
                | Action::LevelSelect {}
        )
    }
}
