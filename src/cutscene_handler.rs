use crate::action::Action;
use crate::handler::Handler;
use crate::process_details::{AddressOffsets, AddressType};
use crate::cutscene_timing_info::{TimingInfo, TimingEntry};
use crate::tracked_memory::TrackedMemory;
use crate::readable_from_path::ReadableFromPath;
use process_memory::{Architecture, ProcessHandle};
use serde::Deserialize;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct CutsceneHandler {
    prompt: TrackedMemory<u8>,
    status: TrackedMemory<u8>,
    timeline: TrackedMemory<f32>,
    length: TrackedMemory<f32>,
    id: TrackedMemory<u32>,
    handle: ProcessHandle,
    blacklist: HashMap<u32, BlacklistEntry>,
    timing_info: TimingInfo,
    total_time_skipped_rta: f32,
    total_time_skipped_igt: f32,
    skipping_cutscene: Option<TimingEntry>,
    skip_time: Option<f32>,
    fadeout_start: Option<Instant>,
}

impl CutsceneHandler {
    pub fn new(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
        blacklist_location: &String,
        timing_info_path: &String,
    ) -> Option<CutsceneHandler> {
        println!("Loading cutscene skipper handler...");

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
            length: TrackedMemory::<f32>::new(
                0.0,
                address_offsets.get(&AddressType::CutsceneLength)?.clone(),
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
            blacklist: Blacklist::from_path(blacklist_location, &String::from("cutscene blacklist")),
            timing_info: TimingInfo::from_path(timing_info_path, &String::from("cutscene timing info")),
            total_time_skipped_rta: 0.0,
            total_time_skipped_igt: 0.0,
            skipping_cutscene: None,
            skip_time: None,
            fadeout_start: None,
        })
    }

    fn skip(&mut self) -> Result<(), Box<dyn Error>> {
        let valid_cutscene = || -> Result<(), Box<dyn Error>> {
            self.prompt.fetch_from_game(self.handle)?;
            self.status.fetch_from_game(self.handle)?;
            self.timeline.fetch_from_game(self.handle)?;
            self.length.fetch_from_game(self.handle)?;
            self.id.fetch_from_game(self.handle)?;
            Ok(())
        }();
        if valid_cutscene.is_err() {
            // We don't generate an error here because if people bind to space we don't want to
            // print an error every time they jump
            return Ok(());
        }

        if self.status.data == 5 {
            // Already skipping, whether via this tool or regular skip
            return Ok(());
        }

        let mut delay = 6.0;
        match self.blacklist.get(&self.id.data) {
            Some(blacklist_entry) => {
                delay = blacklist_entry.skip_delay;
            }
            None => {}
        }

        if self.prompt.data != 1 {
            return Err(CutsceneError::new("No loading prompt, not skipping.").into());
        }

        if delay == 20000.0 {
            return Err(CutsceneError::new(
                "Cutscene blacklisted, cannot be skipped to prevent issues.",
            )
            .into());
        }

        if self.timeline.data <= delay {
            let remaining = delay - self.timeline.data;
            return Err(CutsceneError::new(
                format!("Too early in cutscene.  Skipable in {} seconds.", remaining).as_str(),
            )
            .into());
        }

        let cutscene_info = self.timing_info.find(&self.id.data);
        if cutscene_info.is_none() {
            println!("No cutscene timing info for cutscene {}, not skipping!", self.id.data);
            return Ok(())
        }

        let cutscene_info = cutscene_info.unwrap().clone();

        let timing_threshold = 0.1;
        if
            (cutscene_info.in_game_time - cutscene_info.real_time).abs() > timing_threshold
            && cutscene_info.in_game_time > timing_threshold
        {
            println!("Cutscene timing info is inconsistent between in game and real time for cutscene {}, not skipping! Report these ids along with which cutscene you were attempting to skip!", self.id.data);
        }

        self.start_fadeout(cutscene_info)?;
        println!("Skipping...");

        Ok(())
    }

    fn start_fadeout(&mut self, cutscene_info: TimingEntry) -> Result<(), Box<dyn Error>> {
        self.status.data = 5;
        if self.status.apply_to_game(self.handle).is_err() {
            return Err(CutsceneError::new("Failed to set cutscene state!").into());
        }

        self.skipping_cutscene = Some(cutscene_info.clone());
        self.skip_time = Some(self.timeline.data);
        // For some reason the timeline time changes during the fadeout so we time it manually
        self.fadeout_start = Some(Instant::now());

        Ok(())
    }

    fn is_fadeout_finished(&mut self) -> bool {
        let valid_cutscene = || -> Result<(), Box<dyn Error>> {
            self.prompt.fetch_from_game(self.handle)?;
            self.status.fetch_from_game(self.handle)?;
            self.timeline.fetch_from_game(self.handle)?;
            self.length.fetch_from_game(self.handle)?;
            self.id.fetch_from_game(self.handle)?;
            Ok(())
        }();

        let cutscene_info = self.skipping_cutscene.as_ref().unwrap();

        return valid_cutscene.is_err() || !cutscene_info.ids.contains(&self.id.data);
    }

    fn finish_fadeout(&mut self) {
        let cutscene_info = self.skipping_cutscene.as_ref().unwrap();

        let fadeout_time = Instant::now()
            .duration_since(self.fadeout_start.unwrap())
            .as_secs_f32();

        let time_skipped_rta = (
            cutscene_info.real_time
            - self.skip_time.unwrap()
            - fadeout_time
        )
            .max(0.0);

        let time_skipped_igt = (
            cutscene_info.in_game_time
            - self.skip_time.unwrap()
            - fadeout_time
        )
            .max(0.0);

        println!("Skipped cutscene. Saved {} seconds RTA, {} seconds IGT.", time_skipped_rta, time_skipped_igt);

        self.total_time_skipped_rta += time_skipped_rta;
        self.total_time_skipped_igt += time_skipped_igt;

        self.skipping_cutscene = None;
        self.skip_time = None;
        self.fadeout_start = None;
    }
}

impl Handler for CutsceneHandler {
    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        if self.skipping_cutscene.is_some() {
            if self.is_fadeout_finished() {
                self.finish_fadeout();
            }

        }

        Ok(())
    }
    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>> {
        match action {
            Action::SkipCutscene {} => self.skip(),
            Action::ResetSkipCutsceneTracker {} => {
                println!("Skipped a total of {} seconds RTA, {} seconds IGT", self.total_time_skipped_rta, self.total_time_skipped_igt);
                self.total_time_skipped_rta = 0.0;
                self.total_time_skipped_igt = 0.0;
                println!("Reset skip cutscene tracker");
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug)]
struct CutsceneError {
    message: String,
}

impl CutsceneError {
    pub fn new(message: &str) -> CutsceneError {
        CutsceneError {
            message: message.to_string(),
        }
    }
}

impl fmt::Display for CutsceneError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Cutscene Error: {}", self.message)
    }
}

impl Error for CutsceneError {}


pub type Blacklist = HashMap<u32, BlacklistEntry>;

impl ReadableFromPath for Blacklist {}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct BlacklistEntry {
    pub skip_delay: f32,
}
