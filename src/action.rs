use serde::Deserialize;

fn default_distance() -> f32 {
    100.0
}

#[derive(Debug, Copy, Clone, Deserialize)]
pub enum Action {
    ToggleActive {},
    StorePosition {},
    RestorePosition {},
    SkipCutscene {},
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
}
