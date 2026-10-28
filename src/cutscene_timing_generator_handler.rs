//! Records cutscene timings, by watching the cutscene's memory and asking LiveSplit Server for the time over TCP
//! (`getcurrenttime`), and writes the timing file after each cutscene. LiveSplit Server is connected to when its time
//! is wanted, so it can be started (or restarted) after tomb-helper; a cutscene it couldn't time isn't recorded.

use crate::action::Action;
use crate::handler::Handler;
use crate::find_process::Connection;
use crate::process_details::AddressType;
use crate::tracked_memory::TrackedMemory;
use crate::cutscene_timing_info::{TimingInfo, TimingEntry};
use crate::readable_from_path::ReadableFromPath;
use process_memory::ProcessHandle;
use std::error::Error;
use std::time::{Duration, Instant};
use std::net::TcpStream;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::collections::HashSet;
use std::fmt;

// `CutscenePrompt` once the cutscene can be skipped, which is when it's recorded as skippable. Not the value the
// cutscene handler waits for (`PROMPT_SHOWN`, 1).
const PROMPT_SKIPPABLE: u8 = 2;
// How long LiveSplit Server has to answer
const LIVESPLIT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub struct CutsceneTimingGeneratorHandler {
    timeline: TrackedMemory<f32>,
    id: TrackedMemory<u32>,
    handle: ProcessHandle,
    // LiveSplit's time when the cutscene started, None if it couldn't be read (so the cutscene isn't recorded)
    current_start_game_time: Option<std::time::Duration>,
    current_start_real_time: Option<Instant>,
    // Set while in a cutscene
    current_timeline: Option<f32>,
    current_id_list: HashSet<u32>,
    skip_game_time: Option<std::time::Duration>,
    skip_real_time: Option<Instant>,
    livesplit: LiveSplit,
    timing_info_path: PathBuf,
    timing_info: TimingInfo,
    prompt: TrackedMemory<u8>,
}

impl CutsceneTimingGeneratorHandler {
    pub fn new(connection: &Connection, timing_info_path: &str, livesplit_port: u32) -> Option<CutsceneTimingGeneratorHandler> {
        println!("Loading cutscene timing generation handler...");

        let mut handler = CutsceneTimingGeneratorHandler {
            timeline: TrackedMemory::find(connection, AddressType::CutsceneTimeline)?,
            id: TrackedMemory::find(connection, AddressType::CutsceneId)?,
            handle: connection.process,
            current_start_game_time: None,
            current_start_real_time: None,
            current_timeline: None,
            current_id_list: HashSet::new(),
            skip_game_time: None,
            skip_real_time: None,
            livesplit: LiveSplit { port: livesplit_port, connection: None },
            timing_info_path: PathBuf::from(timing_info_path),
            timing_info: TimingInfo::from_path(timing_info_path, "cutscene timing info"),
            prompt: TrackedMemory::find(connection, AddressType::CutscenePrompt)?,
        };
        // Said now, rather than only once a cutscene starts
        if let Err(e) = handler.livesplit.time() {
            eprintln!("Error: {}", e);
        }
        Some(handler)
    }

    fn started_cutscene(&mut self) -> Result<(), Box<dyn Error>> {
        let game_time = self.livesplit.time();
        self.current_timeline = Some(self.timeline.data);
        self.current_start_game_time = game_time.as_ref().ok().copied();
        self.current_start_real_time = Some(Instant::now());

        println!(
            "Started cutscene: id: {}, timeline time: {}",
            self.id.data,
            self.timeline.data,
        );
        game_time.map(|_| ()).map_err(Into::into)
    }

    fn finished_cutscene(&mut self) -> Result<(), Box<dyn Error>> {
        let end_game_time = self.livesplit.time();
        let end_real_time = Instant::now();
        // It's over, whether or not it can be recorded
        let ids = std::mem::take(&mut self.current_id_list);
        let timeline = self.current_timeline.take();
        let (start_game_time, start_real_time) = (self.current_start_game_time.take(), self.current_start_real_time.take());
        let (skip_game_time, skip_real_time) = (self.skip_game_time.take(), self.skip_real_time.take());

        let not_recorded = |why: &str| -> Box<dyn Error> {
            CutsceneTimingError::new(&format!("Not recording cutscene {:?}: {}", ids, why)).into()
        };
        let (Some(start_game_time), Some(start_real_time)) = (start_game_time, start_real_time) else {
            return Err(not_recorded("LiveSplit's time couldn't be read when it started"));
        };
        if skip_real_time.is_some() && skip_game_time.is_none() {
            return Err(not_recorded("LiveSplit's time couldn't be read when it became skippable"));
        }
        let end_game_time = end_game_time.map_err(|e| not_recorded(&e))?;
        // LiveSplit's time goes back when its timer's reset
        let since_start = |time: Duration| time.checked_sub(start_game_time).ok_or_else(|| not_recorded("LiveSplit's time went back"));
        let length_game_time = since_start(end_game_time)?;
        let length_real_time = end_real_time.duration_since(start_real_time);
        let skippable_at_in_game_time = skip_game_time.map(since_start).transpose()?.map(|time| time.as_secs_f32());
        let skippable_at_real_time = skip_real_time.map(|time| time.duration_since(start_real_time).as_secs_f32());

        let new_entry = TimingEntry{
            ids: ids.clone(),
            real_time: length_real_time.as_secs_f32(),
            in_game_time: length_game_time.as_secs_f32(),
            skippable_at_in_game_time,
            skippable_at_real_time,
        };

        self.timing_info.cutscenes.retain(|e| e.ids != new_entry.ids);
        self.timing_info.cutscenes.push(new_entry);

        self.timing_info.write_to_file(&self.timing_info_path)?;

        println!(
            "Wrote cutscene to file: ids: {:?}, duration: {:?}s IGT, {:?}s RTA, skippable at {:?}s IGT, {:?}s RTA, {:?} timeline length",
            ids,
            length_game_time.as_secs_f32(),
            length_real_time.as_secs_f32(),
            skippable_at_in_game_time,
            skippable_at_real_time,
            timeline.unwrap_or_default(),
        );
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
            let new_timeline = self.current_timeline.is_some_and(|timeline| self.timeline.data < timeline);
            if new_timeline {
                // The next one starts now even if this one couldn't be recorded
                let finished = self.finished_cutscene();
                let started = self.started_cutscene();
                finished.and(started)?;
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

            if inserted {
                println!(
                    "Added id {}, timeline time: {}",
                    self.id.data,
                    self.timeline.data,
                );
            }

            if self.skip_game_time.is_none() && self.skip_real_time.is_none() {
                self.prompt.fetch_from_game(self.handle)?;
                if self.prompt.data == PROMPT_SKIPPABLE {
                    let game_time = self.livesplit.time();
                    self.skip_game_time = game_time.as_ref().ok().copied();
                    self.skip_real_time = Some(Instant::now());
                    println!("skippable at: {:?} IGT, {:?} RTA", self.skip_game_time, self.skip_real_time);
                    game_time?;
                }
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
    fn handle_action(&mut self, _action: Action) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
}

// LiveSplit Server, connected to when its time is first wanted, and again after the connection's lost
#[derive(Debug)]
struct LiveSplit {
    port: u32,
    connection: Option<TcpStream>,
}

impl LiveSplit {
    // Its current time (the game time, as LiveSplit should be comparing against it)
    fn time(&mut self) -> Result<Duration, String> {
        let mut connection = match self.connection.take() {
            Some(connection) => connection,
            None => connect(self.port).map_err(|e| {
                format!(
                    "couldn't connect to LiveSplit Server on port {} ({}), so cutscenes can't be timed until it's started \
                    (right click LiveSplit, Control, Start Server)",
                    self.port, e
                )
            })?,
        };
        let reply = current_time(&mut connection).map_err(|e| format!("LiveSplit Server didn't say what the time is ({})", e))?;
        // Kept only while it answers, so it's connected to again after LiveSplit's restarted
        self.connection = Some(connection);
        parse_time(&reply)
    }
}

fn connect(port: u32) -> io::Result<TcpStream> {
    let connection = TcpStream::connect(format!("127.0.0.1:{}", port))?;
    connection.set_read_timeout(Some(LIVESPLIT_TIMEOUT))?;
    Ok(connection)
}

// Asks LiveSplit Server for its current time, and reads the answer
fn current_time(connection: &mut TcpStream) -> io::Result<String> {
    connection.write_all(b"getcurrenttime\r\n")?;
    let mut reply = String::new();
    if BufReader::new(&*connection).read_line(&mut reply)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "it closed the connection"));
    }
    Ok(reply)
}

// A time as LiveSplit Server gives it: hours, minutes and seconds (e.g. "1:23:45.67"), of which only the seconds are
// always there
fn parse_time(text: &str) -> Result<Duration, String> {
    let invalid = || format!("LiveSplit Server sent a time that isn't one: {:?}", text.trim());
    let units = text.trim().split(':').rev().map(|unit| unit.parse::<f32>()).collect::<Result<Vec<_>, _>>().map_err(|_| invalid())?;
    if units.len() > 3 {
        return Err(invalid());
    }
    let seconds = units.iter().zip([1.0, 60.0, 60.0 * 60.0]).map(|(unit, scale)| unit * scale).sum::<f32>();
    // Negative while LiveSplit counts down to its start
    Duration::try_from_secs_f32(seconds).map_err(|_| invalid())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn livesplit_times_are_read() {
        assert_eq!(parse_time("1:02:03.5\r\n"), Ok(Duration::from_secs_f32(3723.5)));
        assert_eq!(parse_time("2:03.25"), Ok(Duration::from_secs_f32(123.25)));
        assert_eq!(parse_time("4.5"), Ok(Duration::from_secs_f32(4.5)));
        assert!(parse_time("").is_err());
        assert!(parse_time("-1:00.00").is_err());
        assert!(parse_time("1:2:3:4").is_err());
    }
}
