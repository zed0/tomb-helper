use serde::{Serialize, Deserialize};
use std::error::Error;
use std::path::PathBuf;
use std::collections::HashSet;
use std::fs;
use crate::readable_from_path::ReadableFromPath;

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct TimingInfo {
    pub cutscenes: Vec<TimingEntry>,
}

impl TimingInfo {
    pub fn write_to_file(&self, timing_info_path: &PathBuf) -> Result<(), Box<dyn Error>> {
        fs::write(timing_info_path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn find(&self, cutscene_id: &u32) -> Option<&TimingEntry> {
        self.cutscenes.iter()
            .find(|e| e.ids.contains(cutscene_id))
    }
}

impl ReadableFromPath for TimingInfo {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TimingEntry {
    pub ids: HashSet<u32>,
    pub real_time: f32,
    pub in_game_time: f32,
}
