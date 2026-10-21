//! `tomb-helper port`: adding a new patch of a game, or a new game, step by step, asking for what it needs from the
//! game along the way. It runs the same steps as the separate commands: `update-offsets` (with the newest dump of the
//! game's last build as `--from`), `check --fit` if any checks fail, `check --snapshot`, and `watch --save` for the
//! values and layout fields the game's other builds have witnesses for (or, for a new game, a few layout fields), so
//! the next patch can be ported from this one.

use anyhow::Result;
use std::collections::BTreeSet;
use std::io::{self, BufRead, Write};

use crate::offsets_file::{known_process_details, OffsetsSource};
use crate::process_details::ProcessDetails;
use crate::update_offsets::NewGame;

// Recording witnesses: time to switch back to the game (it pauses while alt-tabbed), and how long to record
const SWITCH_SECONDS: u64 = 10;
const RECORD_SECONDS: u64 = 8;
// Layout fields to record for a game whose other builds have none, one from each structure that can be found
const DEFAULT_LAYOUT_FIELDS: [&str; 4] = ["instance.position", "camera.view", "proxy.in_use_size", "collision_mesh.triangles"];

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

    step("Recording what the next patch needs");
    let (values, fields) = witnessed(&details);
    if values.is_empty() {
        println!("There are no values to record for {} (its other builds have no witnesses).", details.name);
    }
    for chunk in values.chunks(crate::watch::MAX_WATCHES) {
        let names: Vec<&str> = chunk.iter().map(String::as_str).collect();
        let cutscene = names.iter().any(|n| n.starts_with("Cutscene"));
        let what = if cutscene { "start a cutscene" } else { "move Lara around, with the camera turning" };
        if wait_or_skip(&format!(
            "Recording the instructions that use {}. Press Enter, then switch to the game within {}s and {}.",
            names.join(", "),
            SWITCH_SECONDS,
            what
        )) {
            report(crate::watch::watch(&names, None, RECORD_SECONDS, SWITCH_SECONDS, false, true));
        }
    }
    for chunk in fields.chunks(crate::watch::MAX_WATCHES) {
        let names: Vec<&str> = chunk.iter().map(String::as_str).collect();
        if wait_or_skip(&format!(
            "Recording the instructions that use the layout fields {}. Press Enter, then switch to the game within {}s and \
             move Lara around, aiming and rolling, with the camera turning.",
            names.join(", "),
            SWITCH_SECONDS
        )) {
            report(crate::layout_witness::watch_layout(&names, None, RECORD_SECONDS, SWITCH_SECONDS, true));
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

fn is_known(executable: &str) -> bool {
    known_process_details().iter().any(|d| d.executable_name.eq_ignore_ascii_case(executable))
}

// The values and layout fields to record: those the game's other builds have witnesses for (which is what `--from`
// matches), or for a game with none, some layout fields to start with
fn witnessed(details: &ProcessDetails) -> (Vec<String>, Vec<String>) {
    let others: Vec<ProcessDetails> =
        known_process_details().into_iter().filter(|d| d.executable_name == details.executable_name).collect();
    let values: BTreeSet<String> = others
        .iter()
        .flat_map(|d| d.witnesses.keys())
        .filter(|t| details.address_offsets.contains_key(t))
        .map(|t| format!("{:?}", t))
        .collect();
    let mut fields: BTreeSet<String> = others.iter().flat_map(|d| d.layout_witnesses.keys().cloned()).collect();
    if fields.is_empty() {
        fields = DEFAULT_LAYOUT_FIELDS.iter().map(|f| f.to_string()).collect();
    }
    (values.into_iter().collect(), fields.into_iter().collect())
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
