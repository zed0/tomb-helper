//! `tomb-helper port`: adding a new patch of a game, or a new game, step by step, asking for what it needs from the
//! game along the way. It runs the same steps as the separate commands: `update-offsets` (with the newest dump of the
//! game's last build as `--from`), `check --fit` if any checks fail, `check --snapshot`, `debug find-cutscene-id --save`
//! if the cutscene's id couldn't be derived for sure, `debug flight-fields --save`, and `debug find-fall-start --save`
//! if the fall start isn't known. The dump `update-offsets` keeps is what the next patch is ported from.

use anyhow::Result;
use std::io::{self, BufRead, Write};

use crate::offsets_file::{known_process_details, OffsetsSource};
use crate::process_details::{AddressType, ProcessDetails};
use crate::update_offsets::NewGame;

// Time to switch back to the game for the steps that need it running (it pauses while alt-tabbed)
const SWITCH_SECONDS: u64 = 10;

/// `tomb-helper port [--exe <Game.exe>]`
pub fn port(exe: Option<String>) -> Result<()> {
    println!("Porting tomb-helper to a new patch or game. It takes a few minutes; press Ctrl+C to stop at any point.");
    println!("What it finds is saved to {} as it goes.", OffsetsSource::new().path.display());
    let new_game = match exe {
        Some(executable) if !is_known(&executable) => {
            println!("\n{} isn't a game tomb-helper knows, so this starts an entry for it.", executable);
            let name = ask("What's the game called?", executable.trim_end_matches(".exe"));
            let version = ask("And this build (e.g. its version number)?", "");
            Some(NewGame { executable, name, version: Some(version).filter(|v| !v.is_empty()) })
        }
        _ => None,
    };

    let mut steps = 0;
    let mut step = |title: &str| {
        steps += 1;
        println!("\n=== {}. {} ===", steps, title);
    };
    step("Finding the offsets");
    wait("Start the game, load into a level, and have Lara stand somewhere ordinary (on the ground, not swimming or in a \
          cutscene). The game can stay paused. Press Enter when she's there.");
    let (details, checked) = match crate::update_offsets::run(None, None, false, new_game.as_ref()) {
        Ok(found) => found,
        Err(e) if new_game.is_none() => {
            return Err(e.context("For a game tomb-helper doesn't know yet, run `tomb-helper port --exe <Game.exe>`"))
        }
        Err(e) => return Err(e),
    };

    let mut passing = checked.is_ok();
    if !passing {
        step("Fixing the layout");
        println!("Some checks failed, so searching for where the structures' fields moved to (about 20s for each part of the \
                  layout that fails)...");
        report(crate::fit_layout::fit_layout(&[], None, None, crate::fit_layout::Save::Ask));
        println!("\nChecking again...");
        passing = crate::check::check(None).is_ok();
    }

    step("Taking a snapshot");
    println!("A snapshot of what `check` reads, so later changes to tomb-helper can be tested against this build without \
              the game (`tomb-helper check --replay`).");
    report(crate::snapshot::take(None, None));

    // The code reaches it through a pointer to the record holding it, so it often can't be derived for sure
    let mut details = details;
    if details.unverified.contains_key(&AddressType::CutsceneId) {
        step("Finding the cutscene's id");
        if wait_or_skip(
            "Where the cutscene's id is couldn't be worked out for sure, so it's found from the cutscene timing file while one \
             plays. Start a cutscene (the game can be paused, and switching back here is fine), then press Enter.",
        ) {
            report(crate::cutscene_id::find_cutscene_id(None, true));
            details = refreshed(&details);
        }
    }

    // What flight mode writes through: found from the structures and code, so it's checked for every build
    step("Finding flight mode's fields");
    println!("Her proxy's collision bit and resolved centre, the camera's collision switch and the body list's links, from \
              the game's structures and code (Lara standing, the game can stay paused)...");
    report(crate::flight_fields::find_flight_fields(None, true));
    details = refreshed(&details);

    // The height she last left the ground at, which flight mode moves with her (or she dies from flying down), and her
    // velocity on the way
    if details.layout.player.fall_start.is_none() {
        step("Finding where her fall starts");
        if wait_or_skip(&format!(
            "Where the height she last left the ground at is isn't known, so she's lifted a little twice to find it (and \
             her velocity; she lands where she is). Have Lara standing on open ground, press Enter, then switch to the game \
             within {}s.",
            SWITCH_SECONDS
        )) {
            report(crate::fall_start::find_fall_start(None, SWITCH_SECONDS, true));
            details = refreshed(&details);
        }
    }

    println!("\n=== Done ===");
    println!("{} {} is saved in {}.", details.name, details.version.version, OffsetsSource::new().path.display());
    if passing {
        println!("Every check passes, so the overlay should work: press F9 in game to try it.");
    } else {
        println!("Some checks still fail; those parts of the overlay won't be right until the layout is fixed by hand (see \
                  \"Porting the overlay\" in the README, and `tomb-helper check`).");
    }
    if !details.unverified.is_empty() {
        let mut names: Vec<String> = details.unverified.keys().map(|t| format!("{:?}", t)).collect();
        names.sort();
        println!("Not found for sure, so not used: {}.", names.join(", "));
    }
    println!("A dump of this build is kept in {} for porting the next one.", crate::snapshot::dump_dir().display());
    println!("To share it, send that file's entry for {}, or add it to offsets.json in tomb-helper's source.", details.version.version);
    Ok(())
}

// The version's entry as it's been saved since
fn refreshed(details: &ProcessDetails) -> ProcessDetails {
    known_process_details()
        .into_iter()
        .find(|d| d.executable_name == details.executable_name && d.version.version == details.version.version)
        .unwrap_or_else(|| details.clone())
}

fn is_known(executable: &str) -> bool {
    known_process_details().iter().any(|d| d.executable_name.eq_ignore_ascii_case(executable))
}

// A step that went wrong is reported, and the rest still run
fn report(result: Result<()>) {
    if let Err(e) = result {
        println!("That didn't work: {:#}", e);
    }
}

fn read_line() -> String {
    let mut line = String::new();
    // Ctrl+C or a closed input ends it like Enter would
    let _ = io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}

fn wait(message: &str) {
    print!("{} ", message);
    let _ = io::stdout().flush();
    read_line();
}

// False if they type s to skip it
fn wait_or_skip(message: &str) -> bool {
    print!("\n{} (Or type s and Enter to skip it.) ", message);
    let _ = io::stdout().flush();
    !read_line().eq_ignore_ascii_case("s")
}

fn ask(question: &str, default: &str) -> String {
    if default.is_empty() {
        print!("{} ", question);
    } else {
        print!("{} [{}] ", question, default);
    }
    let _ = io::stdout().flush();
    let answer = read_line();
    if answer.is_empty() {
        default.to_string()
    } else {
        answer
    }
}

/// Asks a yes or no question, yes by default
pub fn confirm(question: &str) -> bool {
    print!("{} [Y/n] ", question);
    let _ = io::stdout().flush();
    !read_line().to_ascii_lowercase().starts_with('n')
}
