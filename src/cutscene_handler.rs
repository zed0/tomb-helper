//! Cutscene skipping (SOTTR): skips a cutscene (by setting its status to `STATUS_SKIPPING`) once its prompt is up and
//! the blacklist's delay for it has passed, and adds up the real and game time saved using the timing info. `skip`
//! returns `Ok` when there's no cutscene, as Space is also jump. A blacklist delay of `NEVER_SKIP` means never skip.

use crate::action::Action;
use crate::components::Components;
use crate::find_process::Connection;
use crate::game::Game;
use crate::handler::Handler;
use crate::process_details::AddressType;
use crate::cutscene_timing_info::{TimingInfo, TimingEntry};
use crate::targets;
use crate::tracked_memory::TrackedMemory;
use crate::readable_from_path::ReadableFromPath;
use process_memory::ProcessHandle;
use serde::Deserialize;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::time::Instant;

// `CutsceneStatus` while it's being skipped (by this or by the game's own skip)
const STATUS_SKIPPING: u8 = 5;
// `CutscenePrompt` while the skip prompt is up
const PROMPT_SHOWN: u8 = 1;
// Seconds into a cutscene before it's skipped, unless the blacklist says otherwise
const DEFAULT_SKIP_DELAY: f32 = 6.0;
// A blacklist `skip_delay` meaning never skip it
const NEVER_SKIP: f32 = 20000.0;
// Seconds the timing info's real and game times can differ by before it's reported as inconsistent
const TIMING_TOLERANCE: f32 = 0.1;
// Seconds after the prompt changes before the game will let you skip, timed by when the values in Cheat Engine change
const PROMPT_DELAY: f32 = 0.23;

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
    // For checking the status looks right before it's written
    game: Game,
    components: Components,
}

impl CutsceneHandler {
    pub fn new(connection: &Connection, blacklist_location: &str, timing_info_path: &str) -> Option<CutsceneHandler> {
        println!("Loading cutscene skipper handler...");

        Some(CutsceneHandler {
            prompt: TrackedMemory::find(connection, AddressType::CutscenePrompt)?,
            status: TrackedMemory::find(connection, AddressType::CutsceneStatus)?,
            timeline: TrackedMemory::find(connection, AddressType::CutsceneTimeline)?,
            length: TrackedMemory::find(connection, AddressType::CutsceneLength)?,
            id: TrackedMemory::find(connection, AddressType::CutsceneId)?,
            handle: connection.process,
            blacklist: Blacklist::from_path(blacklist_location, "cutscene blacklist"),
            timing_info: TimingInfo::from_path(timing_info_path, "cutscene timing info"),
            total_time_skipped_rta: 0.0,
            total_time_skipped_igt: 0.0,
            skipping_cutscene: None,
            skip_time: None,
            fadeout_start: None,
            game: connection.game(),
            components: Components::new(connection.base_address),
        })
    }

    // Reads the cutscene's values, which fails when there's no cutscene
    fn fetch(&mut self) -> Result<(), Box<dyn Error>> {
        self.prompt.fetch_from_game(self.handle)?;
        self.status.fetch_from_game(self.handle)?;
        self.timeline.fetch_from_game(self.handle)?;
        self.length.fetch_from_game(self.handle)?;
        self.id.fetch_from_game(self.handle)?;
        Ok(())
    }

    fn skip(&mut self) -> Result<(), Box<dyn Error>> {
        if self.fetch().is_err() {
            // We don't generate an error here because if people bind to space we don't want to
            // print an error every time they jump
            return Ok(());
        }

        if self.status.data == STATUS_SKIPPING {
            // Already skipping, whether via this tool or regular skip
            return Ok(());
        }

        let delay = self.blacklist.get(&self.id.data).map_or(DEFAULT_SKIP_DELAY, |entry| entry.skip_delay);

        if self.prompt.data != PROMPT_SHOWN {
            return Err(CutsceneError::new("No loading prompt, not skipping.").into());
        }

        if delay == NEVER_SKIP {
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

        let cutscene_info = match self.timing_info.find(&self.id.data) {
            Some(info) => info.clone(),
            None => {
                println!("No cutscene timing info for cutscene {}, not skipping!", self.id.data);
                return Ok(());
            }
        };

        if
            (cutscene_info.in_game_time - cutscene_info.real_time).abs() > TIMING_TOLERANCE
            && cutscene_info.in_game_time > TIMING_TOLERANCE
        {
            println!("Cutscene timing info is inconsistent between in game and real time for cutscene {}, not skipping! Report these ids along with which cutscene you were attempting to skip!", self.id.data);
        }

        // Not through a chain that's wrong for this build
        if let Err(why) = targets::check(&self.game, &mut self.components, AddressType::CutsceneStatus) {
            return Err(CutsceneError::new(&format!("The cutscene's state looks wrong ({}), not skipping.", why)).into());
        }

        self.start_fadeout(cutscene_info)?;
        println!("Skipping...");

        Ok(())
    }

    fn start_fadeout(&mut self, cutscene_info: TimingEntry) -> Result<(), Box<dyn Error>> {
        self.status.data = STATUS_SKIPPING;
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
        let valid_cutscene = self.fetch();

        let cutscene_info = self.skipping_cutscene.as_ref().unwrap();

        return valid_cutscene.is_err() || !cutscene_info.ids.contains(&self.id.data);
    }

    fn finish_fadeout(&mut self) {
        let cutscene_info = self.skipping_cutscene.as_ref().unwrap();
        let skip_time = self.skip_time.unwrap();

        let fadeout_time = Instant::now()
            .duration_since(self.fadeout_start.unwrap())
            .as_secs_f32();

        // From where it was skipped to where it could have been (or else its end, less the fadeout)
        let saved = |skippable_at: Option<f32>, length: f32| {
            match skippable_at {
                Some(skippable_at) => skippable_at - skip_time + PROMPT_DELAY,
                None => length - skip_time - fadeout_time,
            }
            .max(0.0)
        };
        let time_skipped_rta = saved(cutscene_info.skippable_at_real_time, cutscene_info.real_time);
        let time_skipped_igt = saved(cutscene_info.skippable_at_in_game_time, cutscene_info.in_game_time);

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
