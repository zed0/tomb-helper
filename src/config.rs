use serde::{Deserialize};
use livesplit_hotkey::{KeyCode};
use std::{env, fs};
use std::path::PathBuf;

use crate::action::Action;

fn default_config_path() -> PathBuf {
    env::current_exe().unwrap().parent().unwrap().join("tomb-helper.json")
}

pub fn get_config() -> Config {
    let config_path = default_config_path();
    let config;
    if config_path.is_file() {
        println!("Found config at {:?}", config_path.as_os_str());
        config = fs::read_to_string(config_path).expect("Unable to read config file.");
    } else {
        println!("No config found, using defaults. To use a different config create {:?}", config_path);
        config = String::from("{}");
    }
    serde_json::from_str(&config).unwrap()
}

fn default_hotkeys() -> Vec<Hotkey> {
    vec![
        Hotkey::new(KeyCode::F5, Action::ToggleActive{}),
        Hotkey::new(KeyCode::F6, Action::StorePosition{}),
        Hotkey::new(KeyCode::F7, Action::RestorePosition{}),
        Hotkey::new(KeyCode::Space, Action::SkipCutscene{}),
        Hotkey::new(KeyCode::W, Action::Forward{distance: 100.0}),
        Hotkey::new(KeyCode::S, Action::Backward{distance: 100.0}),
        Hotkey::new(KeyCode::A, Action::Left{distance: 100.0}),
        Hotkey::new(KeyCode::D, Action::Right{distance: 100.0}),
        Hotkey::new(KeyCode::Space, Action::Up{distance: 100.0}),
        Hotkey::new(KeyCode::C, Action::Down{distance: 100.0}),
    ]
}

fn default_cutscene_blacklist_file() -> String {
    "https://gist.githubusercontent.com/Atorizil/734a7649471f0fa0a2a9f92a167e294b/raw/bace4c713a0cba7188a9e110a703ffab391f7f31/Blacklist.json".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Hotkey {
    pub key: KeyCode,
    pub action: Action,
}

impl Hotkey {
    fn new(key: KeyCode, action: Action) -> Hotkey {
        Hotkey {
            key,
            action,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_hotkeys")]
    pub hotkeys: Vec<Hotkey>,
    #[serde(default = "default_cutscene_blacklist_file")]
    pub cutscene_blacklist_file: String,
}
