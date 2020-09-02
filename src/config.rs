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
}
