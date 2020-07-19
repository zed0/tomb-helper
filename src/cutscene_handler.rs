use std::fmt;
use std::error::Error;
use std::collections::HashMap;
use process_memory::{ProcessHandle, Architecture};
use serde::{Deserialize};
use reqwest::Url;
use crate::process_details::{AddressType, AddressOffsets};
use crate::tracked_memory::TrackedMemory;
use crate::action::Action;
use crate::handler::Handler;

#[derive(Debug, Clone)]
pub struct CutsceneHandler {
    prompt: TrackedMemory<u8>,
    status: TrackedMemory<u8>,
    timeline: TrackedMemory<f32>,
    id: TrackedMemory<u32>,
    handle: ProcessHandle,
    blacklist: HashMap<u32, BlacklistEntry>,
}

impl CutsceneHandler {
    pub fn new(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
        blacklist_location: &String,
    ) -> Option<CutsceneHandler> {
        Some(CutsceneHandler {
            prompt: TrackedMemory::<u8>::new(
                0,
                address_offsets.get(&AddressType::CutscenePrompt)?.clone(),
                *arch,
                *base_addr,
            ),
            status: TrackedMemory::<u8>::new(
                0,
                address_offsets.get(&AddressType::CutsceneStatus)?.clone(),
                *arch,
                *base_addr,
            ),
            timeline: TrackedMemory::<f32>::new(
                0.0,
                address_offsets.get(&AddressType::CutsceneTimeline)?.clone(),
                *arch,
                *base_addr,
            ),
            id: TrackedMemory::<u32>::new(
                0,
                address_offsets.get(&AddressType::CutsceneId)?.clone(),
                *arch,
                *base_addr,
            ),
            handle: *handle,
            blacklist: get_blacklist(blacklist_location),
        })
    }

    fn skip(&mut self) -> Result<(), Box<dyn Error>> {
        let valid_cutscene = || -> Result<(), Box<dyn Error>> {
            self.prompt.fetch_from_game(self.handle)?;
            self.status.fetch_from_game(self.handle)?;
            self.timeline.fetch_from_game(self.handle)?;
            self.id.fetch_from_game(self.handle)?;
            Ok(())
        }();
        if valid_cutscene.is_err() {
            // We don't generate an error here because if people bind to space we don't want to
            // print an error every time they jump
            return Ok(())
        }

        let mut delay = 6.0;
        match self.blacklist.get(&self.id.data) {
            Some(blacklist_entry) => {
                delay = blacklist_entry.skip_delay;
            },
            None => {},
        }

        if self.prompt.data != 1 {
            return Err(CutsceneError::new("No loading prompt, not skipping.").into());
        }

        if delay == 20000.0 {
            return Err(CutsceneError::new("Cutscene blacklisted, cannot be skipped to prevent issues.").into());
        }

        if self.timeline.data <= delay {
            let remaining = delay - self.timeline.data;
            return Err(CutsceneError::new(format!("Too early in cutscene.  Skipable in {} seconds.", remaining).as_str()).into());
        }

        self.status.data = 5;
        if self.status.apply_to_game(self.handle).is_err() {
            return Err(CutsceneError::new("Failed to set cutscene state!").into());
        }
        println!("Skipped cutscene.");
        Ok(())
    }
}

impl Handler for CutsceneHandler {
    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>> {
        match action {
            Action::SkipCutscene{} => self.skip(),
            _ => Ok(()),
        }
    }
}


#[derive(Debug)]
struct CutsceneError {
    message: String,
}

impl CutsceneError {
    pub fn new(
        message: &str,
    ) -> CutsceneError {
        CutsceneError {
            message: message.to_string()
        }
    }
}

impl fmt::Display for CutsceneError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Cutscene Error: {}", self.message)
    }
}

impl Error for CutsceneError {}


#[derive(Debug, Clone, Deserialize)]
pub struct Blacklist {
}

fn get_blacklist(blacklist_location: &String) -> HashMap<u32, BlacklistEntry> {
    println!("Loading cutscene blacklist from {}", blacklist_location);
    let url = Url::parse(blacklist_location).expect("Could not parse cutscene blacklist url");
    let content = reqwest::blocking::get(url).expect("Could not retrieve cutscene blacklist url").text().unwrap();
    return serde_json::from_str(&content).expect("Could not parse cutscene blacklist to expected format");
}

#[derive(Debug, Clone, Deserialize)]
pub struct BlacklistEntry {
    pub skip_delay: f32,
}
