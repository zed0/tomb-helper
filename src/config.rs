use livesplit_hotkey::KeyCode;
use serde::Deserialize;
use std::path::PathBuf;
use std::{env, fs};

use crate::action::Action;

fn default_config_path() -> PathBuf {
    env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("tomb-helper.json")
}

pub fn get_config() -> Config {
    let config_path = default_config_path();
    let config;
    if config_path.is_file() {
        println!("Found config at {:?}", config_path.as_os_str());
        config = fs::read_to_string(config_path).expect("Unable to read config file.");
    } else {
        println!(
            "No config found, using defaults. To use a different config create {:?}",
            config_path
        );
        config = String::from("{}");
    }
    serde_json::from_str(&config).unwrap()
}

fn default_hotkeys() -> Vec<Hotkey> {
    vec![
        Hotkey::new(KeyCode::F5, Action::ToggleActive {}),
        Hotkey::new(KeyCode::F6, Action::StorePosition {}),
        Hotkey::new(KeyCode::F7, Action::RestorePosition {}),
        Hotkey::new(KeyCode::F8, Action::ResetSkipCutsceneTracker {}),
        Hotkey::new(KeyCode::F9, Action::ToggleUI {}),
        Hotkey::new(KeyCode::F10, Action::ToggleCollision {}),
        Hotkey::new(KeyCode::Space, Action::SkipCutscene {}),
        Hotkey::new(KeyCode::W, Action::Forward { distance: 100.0 }),
        Hotkey::new(KeyCode::S, Action::Backward { distance: 100.0 }),
        Hotkey::new(KeyCode::A, Action::Left { distance: 100.0 }),
        Hotkey::new(KeyCode::D, Action::Right { distance: 100.0 }),
        Hotkey::new(KeyCode::Space, Action::Up { distance: 100.0 }),
        Hotkey::new(KeyCode::C, Action::Down { distance: 100.0 }),
    ]
}

fn default_cutscene_blacklist_file() -> String {
    "https://gist.githubusercontent.com/Atorizil/734a7649471f0fa0a2a9f92a167e294b/raw/Blacklist.json".to_string()
}

fn default_cutscene_timing_file() -> String {
    "https://gist.githubusercontent.com/zed0/762a5790501af95189344834bc210616/raw/tomb-helper-timing-info.json".to_string()
}

fn default_cutscene_timing_file_output() -> String {
    env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("tomb-helper-timing-info.json")
        .to_str()
        .unwrap()
        .into()
}

fn default_livesplit_port() -> u32 {
    return 16834;
}

#[derive(Debug, Clone, Deserialize)]
pub struct Hotkey {
    pub key: KeyCode,
    pub action: Action,
}

impl Hotkey {
    fn new(key: KeyCode, action: Action) -> Hotkey {
        Hotkey { key, action }
    }
}

#[derive(Debug, Clone, Deserialize)]
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
        CutsceneTiming::Off{}
    }
}

fn default_object_filters() -> Vec<String> {
    // Interactable objects (including campfires) are shown separately
    vec![]
}

fn default_object_radius() -> f32 {
    5000.0
}

fn default_object_box_size() -> f32 {
    50.0
}

fn default_lara_capsules() -> bool {
    // Slow to draw
    false
}

fn default_lara_probes() -> bool {
    true
}

fn default_lara_proxy() -> bool {
    true
}

fn default_collision() -> bool {
    false
}

fn default_collision_meshes() -> bool {
    true
}

fn default_collision_mesh_radius() -> f32 {
    1500.0
}

fn default_collision_mesh_fade() -> bool {
    true
}

fn default_collision_mesh_fade_strength() -> f32 {
    2.0
}

fn default_collision_mesh_cull_hidden() -> bool {
    true
}

fn default_interactables() -> bool {
    true
}

fn default_npcs() -> bool {
    true
}

fn default_trigger_categories() -> Vec<String> {
    ["checkpoints", "death", "streaming", "combat", "unknown"].iter().map(|c| c.to_string()).collect()
}

fn default_collision_opacity() -> f32 {
    0.25
}

#[derive(Debug, Clone, Deserialize)]
pub struct OverlayConfig {
    // Objects whose placement name contains any of these (case insensitive) get a box. "" matches everything.
    #[serde(default = "default_object_filters")]
    pub object_filters: Vec<String>,
    // Only objects within this distance of Lara are shown
    #[serde(default = "default_object_radius")]
    pub object_radius: f32,
    // Half width of the box drawn around objects that don't have a model
    #[serde(default = "default_object_box_size")]
    pub object_box_size: f32,
    // Draw Lara's hit capsules
    #[serde(default = "default_lara_capsules")]
    pub lara_capsules: bool,
    // Draw the ground probes the game casts for Lara
    #[serde(default = "default_lara_probes")]
    pub lara_probes: bool,
    // Draw the capsule the game moves Lara with
    #[serde(default = "default_lara_proxy")]
    pub lara_proxy: bool,
    // Draw anything to do with collision at all (to start with), whatever the settings for each part
    #[serde(default = "default_collision")]
    pub collision: bool,
    // Draw the level's collision near Lara that she collides with (to start with, it can be toggled)
    #[serde(default = "default_collision_meshes")]
    pub collision_meshes: bool,
    // How far from Lara to draw them (to start with, they can be changed)
    #[serde(default = "default_collision_mesh_radius")]
    pub collision_mesh_radius: f32,
    #[serde(default = "default_collision_mesh_radius")]
    pub detailed_collision_mesh_radius: f32,
    // Also draw the level's collision meshes Lara doesn't collide with (to start with, they can be toggled)
    #[serde(default)]
    pub detailed_collision_meshes: bool,
    // Draw the collision meshes fainter further from Lara, by (1 - distance / radius) ^ strength (to start with)
    #[serde(default = "default_collision_mesh_fade")]
    pub collision_mesh_fade: bool,
    #[serde(default = "default_collision_mesh_fade_strength")]
    pub collision_mesh_fade_strength: f32,
    // Show boxes round things Lara can interact with, and trigger volumes (to start with)
    #[serde(default = "default_interactables")]
    pub interactables: bool,
    #[serde(default)]
    pub triggers: bool,
    // Show boxes round characters and animals (to start with)
    #[serde(default = "default_npcs")]
    pub npcs: bool,
    // Which kinds of trigger to show (to start with), see `TriggerCategory::name`
    #[serde(default = "default_trigger_categories")]
    pub trigger_categories: Vec<String>,
    // Only draw collision triangles that aren't hidden behind others (to start with)
    #[serde(default = "default_collision_mesh_cull_hidden")]
    pub collision_mesh_cull_hidden: bool,
    // Scales how opaque the collision meshes and Lara's collision are drawn, 0 to 1 (to start with)
    #[serde(default = "default_collision_opacity")]
    pub collision_opacity: f32,
    // Open a console in the game for the overlay's logs
    #[serde(default)]
    pub console: bool,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        serde_json::from_str("{}").unwrap()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_hotkeys")]
    pub hotkeys: Vec<Hotkey>,
    #[serde(default = "default_cutscene_blacklist_file")]
    pub cutscene_blacklist_file: String,
    #[serde(default = "default_cutscene_timing_file")]
    pub cutscene_timing_file: String,
    #[serde(default)]
    pub record_cutscene_timing: CutsceneTiming,
    #[serde(default)]
    pub force_version: Option<String>,
    #[serde(default)]
    pub overlay: OverlayConfig,
}
