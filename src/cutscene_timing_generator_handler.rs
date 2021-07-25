use crate::action::Action;
use crate::handler::Handler;
use crate::process_details::{AddressOffsets, AddressType};
use crate::tracked_memory::TrackedMemory;
use crate::cutscene_timing_info::{TimingInfo, TimingEntry};
use crate::readable_from_path::ReadableFromPath;
use process_memory::{Architecture, ProcessHandle};
use std::error::Error;
use std::time::{Duration, Instant};
use std::net::TcpStream;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::collections::HashSet;
use std::{fmt, iter};

#[derive(Debug)]
pub struct CutsceneTimingGeneratorHandler {
    timeline: TrackedMemory<f32>,
    id: TrackedMemory<u32>,
    handle: ProcessHandle,
    current_start_game_time: Option<std::time::Duration>,
    current_start_real_time: Option<Instant>,
    current_timeline: Option<f32>,
    current_id_list: HashSet<u32>,
    skip_game_time: Option<std::time::Duration>,
    skip_real_time: Option<Instant>,
    livesplit_connection: TcpStream,
    timing_info_path: PathBuf,
    timing_info: TimingInfo,
    prompt: TrackedMemory<u8>,
}

impl CutsceneTimingGeneratorHandler {
    pub fn new(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
        timing_info_path: &String,
        livesplit_port: &u32,
    ) -> Option<CutsceneTimingGeneratorHandler> {
        println!("Loading cutscene timing generation handler...");

        Some(CutsceneTimingGeneratorHandler {
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
            current_start_game_time: None,
            current_start_real_time: None,
            current_timeline: None,
            current_id_list: HashSet::new(),
            skip_game_time: None,
            skip_real_time: None,
            livesplit_connection: TcpStream::connect(
                format!("127.0.0.1:{}", *livesplit_port)
            ).unwrap(),
            timing_info_path: PathBuf::from(timing_info_path),
            timing_info: TimingInfo::from_path(timing_info_path, &String::from("cutscene timing info")),
            prompt: TrackedMemory::<u8>::new(
                0,
                address_offsets.get(&AddressType::CutscenePrompt)?.clone(),
                *arch,
                *base_addr,
            ),
        })
    }

    fn get_livesplit_time(&mut self) -> Result<Duration, Box<dyn Error>> {
        self.livesplit_connection.write(b"getcurrenttime\r\n").unwrap();
        let mut reader = BufReader::new(self.livesplit_connection.try_clone().unwrap());
        let mut buffer = String::new();
        reader.read_line(&mut buffer).unwrap();
        let hours;
        let minutes;
        let seconds;
        let units = buffer
            .trim()
            .split(':')
            .map(|s| s.parse().unwrap())
            .rev()
            .chain(iter::repeat(0.0))
            .take(3)
            .collect::<Vec<f32>>();

        match units.as_slice() {
            [s,m,h,..] => {
                hours = *h;
                minutes = *m;
                seconds = *s;
            },
            _ => return Err(CutsceneTimingError::new("invalid time from livesplit").into()),
        }

        return Ok(Duration::from_secs_f32(
            seconds
            + (60.0 * minutes)
            + (60.0 * 60.0 * hours)
        ));
    }

    fn started_cutscene(&mut self) -> Result<(), Box<dyn Error>> {
        self.current_timeline = Some(self.timeline.data);
        self.current_start_game_time = Some(self.get_livesplit_time()?);
        self.current_start_real_time = Some(Instant::now());

        println!(
            "Started cutscene: id: {}, timeline time: {}",
            self.id.data,
            self.current_timeline.unwrap(),
        );
        Ok(())
    }

    fn finished_cutscene(&mut self) -> Result<(), Box<dyn Error>> {
        let end_game_time = self.get_livesplit_time()?;
        let length_game_time = end_game_time - self.current_start_game_time.unwrap();

        let end_real_time = Instant::now();
        let length_real_time = end_real_time.duration_since(self.current_start_real_time.unwrap());

        let skippable_at_in_game_time = match self.skip_game_time {
            Some(skip_game_time) => Some((skip_game_time - self.current_start_game_time.unwrap()).as_secs_f32()),
            _ => None,
        };
        let skippable_at_real_time = match self.skip_real_time {
            Some(skip_real_time) => Some(skip_real_time.duration_since(self.current_start_real_time.unwrap()).as_secs_f32()),
            _ => None,
        };

        let new_entry = TimingEntry{
            ids: self.current_id_list.clone(),
            real_time: length_real_time.as_secs_f32(),
            in_game_time: length_game_time.as_secs_f32(),
            skippable_at_in_game_time: skippable_at_in_game_time,
            skippable_at_real_time: skippable_at_real_time,
        };

        self.timing_info.cutscenes.retain(|e| e.ids != new_entry.ids);
        self.timing_info.cutscenes.push(new_entry);

        self.timing_info.write_to_file(&self.timing_info_path)?;

        println!(
            "Wrote cutscene to file: ids: {:?}, duration: {:?}s IGT, {:?}s RTA, skippable at {:?}s IGT, {:?}s RTA, {:?} timeline length",
            self.current_id_list,
            length_game_time.as_secs_f32(),
            length_real_time.as_secs_f32(),
            skippable_at_in_game_time,
            skippable_at_real_time,
            self.current_timeline.unwrap(),
        );

        self.current_timeline = None;
        self.current_id_list = HashSet::new();
        self.current_start_game_time = None;
        self.current_start_real_time = None;
        self.skip_game_time = None;
        self.skip_real_time = None;
        Ok(())
    }

    fn update_cutscene_tracker(&mut self) -> Result<(), Box<dyn Error>> {
        let was_in_valid_cutscene = self.current_timeline.is_some();
        let mut now_in_valid_cutscene = || -> Result<(), Box<dyn Error>> {
            self.timeline.fetch_from_game(self.handle)?;
            self.id.fetch_from_game(self.handle)?;
            Ok(())
        }().is_ok();
        now_in_valid_cutscene = now_in_valid_cutscene && self.id.data != 0;

        if was_in_valid_cutscene && !now_in_valid_cutscene {
            self.finished_cutscene()?;
        }

        if was_in_valid_cutscene && now_in_valid_cutscene {
            let new_timeline = self.timeline.data < self.current_timeline.unwrap();
            if new_timeline {
                self.finished_cutscene()?;
                self.started_cutscene()?;
            }
        }

        if !was_in_valid_cutscene && now_in_valid_cutscene {
            self.started_cutscene()?;
        }

        if now_in_valid_cutscene {
            // Cutscenes can have multiple ids so we record all the ids.
            // Seems to change at camera changes and some other places.
            let inserted = self.current_id_list.insert(self.id.data);
            self.current_timeline = Some(self.timeline.data);

            if self.skip_game_time.is_none() && self.skip_real_time.is_none() {
                self.prompt.fetch_from_game(self.handle)?;
                if self.prompt.data == 2 {
                    self.skip_game_time = Some(self.get_livesplit_time()?);
                    self.skip_real_time = Some(Instant::now());
                    println!("skippable at: {:?} IGT, {:?} RTA", self.skip_game_time, self.skip_real_time);
                }
            }

            if inserted {
                println!(
                    "Added id {}, timeline time: {}",
                    self.id.data,
                    self.timeline.data,
                );
            }
        }
        Ok(())
    }
}

impl Handler for CutsceneTimingGeneratorHandler {
    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        self.update_cutscene_tracker()?;
        Ok(())
    }
    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>> {
        match action {
            _ => Ok(()),
        }
    }
}

#[derive(Debug)]
struct CutsceneTimingError {
    message: String,
}

impl CutsceneTimingError {
    pub fn new(message: &str) -> CutsceneTimingError {
        CutsceneTimingError {
            message: message.to_string(),
        }
    }
}

impl fmt::Display for CutsceneTimingError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Cutscene Error: {}", self.message)
    }
}

impl Error for CutsceneTimingError {}
