//! `tomb-helper.json` next to the exe: hotkeys (`default_hotkeys`), the cutscene timing and blacklist files (gist URLs
//! by default), whether to record cutscene timings (`CutsceneTiming`), and the overlay's settings (`OverlayConfig`, the
//! overlay window's settings to start with).

use crate::action::Action;
use crate::hotkeys::KeyCode;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::{env, fs};

fn exe_dir() -> PathBuf {
    env::current_exe().unwrap().parent().unwrap().to_path_buf()
}

fn default_config_path() -> PathBuf {
    exe_dir().join("tomb-helper.json")
}

pub fn get_config() -> Config {
    read_config(true)
}

fn read_config(announce: bool) -> Config {
    let config_path = default_config_path();
    if !config_path.is_file() {
        if announce {
            println!("No config found, using defaults. To use a different config create {:?}", config_path);
        }
        return Config::default();
    }
    if announce {
        println!("Found config at {:?}", config_path.as_os_str());
    }
    match parse(&config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Error: {} isn't a valid config: {}", config_path.display(), e);
            eprintln!("Fix it, or delete it to use the defaults");
            std::process::exit(1);
        }
    }
}

fn parse(path: &Path) -> Result<Config, Box<dyn Error>> {
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

/// Where the tools keep dumps of game builds and snapshots: `data_dir` in the config, else `tomb-helper-data` next to
/// the exe
pub fn data_dir() -> PathBuf {
    read_config(false).data_dir.map(PathBuf::from).unwrap_or_else(|| exe_dir().join("tomb-helper-data"))
}

fn default_hotkeys() -> Vec<Hotkey> {
    vec![
        Hotkey::new(KeyCode::F4, Action::LevelSelect {}),
        Hotkey::new(KeyCode::F5, Action::ToggleActive {}),
        Hotkey::new(KeyCode::F6, Action::StorePosition {}),
        Hotkey::new(KeyCode::F7, Action::RestorePosition {}),
        Hotkey::new(KeyCode::F8, Action::ResetSkipCutsceneTracker {}),
        Hotkey::new(KeyCode::F9, Action::ToggleUI {}),
        Hotkey::new(KeyCode::F10, Action::ToggleOverlay {}),
        Hotkey::new(KeyCode::F11, Action::ToggleSettingsWindow {}),
        Hotkey::new(KeyCode::Space, Action::SkipCutscene {}),
        Hotkey::new(KeyCode::W, Action::Forward { distance: 100.0 }),
        Hotkey::new(KeyCode::S, Action::Backward { distance: 100.0 }),
        Hotkey::new(KeyCode::A, Action::Left { distance: 100.0 }),
        Hotkey::new(KeyCode::D, Action::Right { distance: 100.0 }),
        Hotkey::new(KeyCode::Space, Action::Up { distance: 100.0 }),
        Hotkey::new(KeyCode::C, Action::Down { distance: 100.0 }),
    ]
}

const DEFAULT_CUTSCENE_BLACKLIST_FILE: &str =
    "https://gist.githubusercontent.com/Atorizil/734a7649471f0fa0a2a9f92a167e294b/raw/Blacklist.json";
const DEFAULT_CUTSCENE_TIMING_FILE: &str =
    "https://gist.githubusercontent.com/zed0/762a5790501af95189344834bc210616/raw/tomb-helper-timing-info.json";

fn default_cutscene_timing_file_output() -> String {
    exe_dir().join("tomb-helper-timing-info.json").to_string_lossy().into_owned()
}

// LiveSplit Server's own default
fn default_livesplit_port() -> u32 {
    16834
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hotkey {
    pub key: KeyCode,
    pub action: Action,
}

impl Hotkey {
    fn new(key: KeyCode, action: Action) -> Hotkey {
        Hotkey { key, action }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CutsceneTiming {
    On {
        #[serde(default = "default_cutscene_timing_file_output")]
        timing_file: String,
        #[serde(default = "default_livesplit_port")]
        livesplit_port: u32,
    },
    Off {},
}

impl Default for CutsceneTiming {
    fn default() -> Self {
        CutsceneTiming::Off {}
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    // Objects whose placement name contains any of these (case insensitive) get a box. "" matches everything.
    pub object_filters: Vec<String>,
    // Only objects within this distance of Lara are shown
    pub object_radius: f32,
    // Half width of the box drawn around objects that don't have a model
    pub object_box_size: f32,
    // Draw Lara's hit capsules
    pub lara_capsules: bool,
    // Draw the ground probes the game casts for Lara
    pub lara_probes: bool,
    // Draw the capsule the game moves Lara with
    pub lara_proxy: bool,
    // Draw anything to do with collision at all (to start with), whatever the settings for each part
    pub collision: bool,
    // Draw the level's collision near Lara that she collides with (to start with, it can be toggled)
    pub collision_meshes: bool,
    // How far from Lara to draw them (to start with, they can be changed)
    pub collision_mesh_radius: f32,
    pub detailed_collision_mesh_radius: f32,
    // Also draw the level's collision meshes with any of `collision_mesh_other_kinds` (to start with, they can be
    // toggled)
    pub detailed_collision_meshes: bool,
    // Which kinds of collision (bits of the meshes' filters) those are, all but Lara's (0x2) by default
    pub collision_mesh_other_kinds: u32,
    // Draw the collision meshes, objects and triggers fainter further from Lara, by (1 - distance / radius) ^ strength
    // (to start with), where the radius is the meshes' or `object_radius`. Once only for the meshes, hence the aliases.
    #[serde(alias = "collision_mesh_fade")]
    pub fade: bool,
    #[serde(alias = "collision_mesh_fade_strength")]
    pub fade_strength: f32,
    // Show boxes round things Lara can interact with, and trigger volumes (to start with)
    pub interactables: bool,
    pub triggers: bool,
    // Show boxes round characters and animals (to start with)
    pub npcs: bool,
    // Which kinds of trigger to show (to start with), see `TriggerCategory::name`
    pub trigger_categories: Vec<String>,
    // Choose which triggers to show by what their scripts do rather than by `trigger_categories` (to start with)
    pub triggers_by_action: bool,
    // Which triggers to show by what their scripts do (to start with), see `TRIGGER_ACTIONS`
    pub trigger_actions: Vec<String>,
    // Only draw collision triangles that aren't hidden behind others (to start with)
    pub collision_mesh_cull_hidden: bool,
    // While Lara aims, draw the collision meshes round where she's aiming rather than round her (to start with)
    pub collision_mesh_follow_aim: bool,
    // Scales how opaque the collision meshes and Lara's collision are drawn, 0 to 1 (to start with)
    pub collision_opacity: f32,
    // How opaque trigger volumes are filled, 0 to 1 (0 just draws their outlines) (to start with)
    pub trigger_opacity: f32,
    // Show the overlay's settings window (to start with, `ToggleSettingsWindow` shows and hides it)
    pub settings_window: bool,
    // Open a console in the game for the overlay's logs
    pub console: bool,
    // How the overlay hooks the game's DirectX functions
    pub hooks: OverlayHooks,
}

/// How the overlay hooks the game's DirectX functions (hudhook's `HookMethod`)
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum OverlayHooks {
    /// Patching the start of each function: every call is hooked, but other overlays that patch the same functions can
    /// overwrite it (RivaTuner does, in TR2013)
    Inline,
    /// Replacing the functions' pointers in DirectX's vtables, which other overlays leave alone, but only calls through
    /// those vtables are hooked (not a swap chain another tool has wrapped in its own object)
    #[default]
    VTable,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        OverlayConfig {
            // Interactable objects (including campfires) are shown separately
            object_filters: vec![],
            object_radius: 5000.0,
            object_box_size: 50.0,
            // Slow to draw
            lara_capsules: false,
            lara_probes: true,
            lara_proxy: true,
            collision: false,
            collision_meshes: true,
            collision_mesh_radius: 1500.0,
            detailed_collision_mesh_radius: 1500.0,
            detailed_collision_meshes: false,
            collision_mesh_other_kinds: {
                let all = (1u32 << crate::ui_ipc_data::COLLISION_KINDS) - 1;
                all & !(crate::layout::CollisionInstanceLayout::default().player_kind as u32)
            },
            fade: true,
            fade_strength: 2.0,
            interactables: true,
            triggers: false,
            npcs: true,
            trigger_categories: ["checkpoints", "death", "streaming", "combat", "unknown"].map(String::from).to_vec(),
            triggers_by_action: true,
            trigger_actions: crate::ui_ipc_data::TRIGGER_ACTIONS.map(String::from).to_vec(),
            collision_mesh_cull_hidden: true,
            collision_mesh_follow_aim: false,
            collision_opacity: 0.25,
            trigger_opacity: 0.15,
            settings_window: true,
            console: false,
            hooks: OverlayHooks::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkeys: Vec<Hotkey>,
    pub cutscene_blacklist_file: String,
    pub cutscene_timing_file: String,
    pub record_cutscene_timing: CutsceneTiming,
    pub overlay: OverlayConfig,
    // Where dumps and snapshots go (`data_dir()`)
    pub data_dir: Option<String>,
    // Also print errors that are expected while the game loads a level or exits, like pointer chains that don't resolve
    pub debug_logging: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hotkeys: default_hotkeys(),
            cutscene_blacklist_file: DEFAULT_CUTSCENE_BLACKLIST_FILE.to_string(),
            cutscene_timing_file: DEFAULT_CUTSCENE_TIMING_FILE.to_string(),
            record_cutscene_timing: CutsceneTiming::default(),
            overlay: OverlayConfig::default(),
            data_dir: None,
            debug_logging: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_are_the_defaults() {
        let config: Config = serde_json::from_str(r#"{"overlay": {"object_radius": 100.0, "collision_mesh_fade": false}}"#).unwrap();
        assert_eq!(config.overlay.object_radius, 100.0);
        assert!(!config.overlay.fade);
        assert_eq!(config.overlay.collision_mesh_radius, 1500.0);
        assert_eq!(config.overlay.collision_mesh_other_kinds, 0xFFFD);
        assert_eq!(config.overlay.trigger_categories.len(), 5);
        assert_eq!(config.hotkeys.len(), default_hotkeys().len());
        assert!(config.cutscene_timing_file.starts_with("https://"));
    }

    // The names of an object's settings, and of those in its objects (e.g. "overlay.fade")
    fn setting_names(value: &serde_json::Value, prefix: &str) -> Vec<String> {
        let Some(object) = value.as_object() else {
            return vec![];
        };
        let mut names = vec![];
        for (name, value) in object {
            let name = format!("{}{}", prefix, name);
            names.extend(setting_names(value, &format!("{}.", name)));
            names.push(name);
        }
        names.sort();
        names
    }

    #[test]
    fn the_readme_has_every_default() {
        let readme = include_str!("../README.md").replace("\r\n", "\n");
        let block = readme.split("The default configuration is:\n```json\n").nth(1).unwrap().split("```").next().unwrap();
        let documented: serde_json::Value = serde_json::from_str(block).unwrap();
        let defaults = serde_json::to_value(Config::default()).unwrap();
        assert_eq!(setting_names(&documented, ""), setting_names(&defaults, ""), "the README's defaults have other settings");
        // Through a Config, as the defaults' floats are f32s
        let read = serde_json::to_value(serde_json::from_value::<Config>(documented).unwrap()).unwrap();
        assert_eq!(read, defaults, "the README's defaults aren't the defaults");
    }

    #[test]
    fn overlay_hooks_default_to_vtable() {
        assert_eq!(OverlayConfig::default().hooks, OverlayHooks::VTable);
        let inline: OverlayConfig = serde_json::from_str(r#"{"hooks": "inline"}"#).unwrap();
        assert_eq!(inline.hooks, OverlayHooks::Inline);
        let vtable: OverlayConfig = serde_json::from_str(r#"{"hooks": "vtable"}"#).unwrap();
        assert_eq!(vtable.hooks, OverlayHooks::VTable);
    }
}
