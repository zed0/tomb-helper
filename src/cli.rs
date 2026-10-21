//! The command line tools (`tomb-helper <command>`); with no command tomb-helper runs normally (`main`). Porting a new
//! build or game takes `update-offsets`, `check` and `watch`; the tools for digging into the game (or into tomb-helper)
//! are under the hidden `debug`.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::Path;

use crate::offsets_file::{known_process_details, to_json, OffsetsFile, OffsetsSource, VersionEntry};
use crate::watch::{Breakpoint, Dump};

#[derive(Parser)]
#[command(name = "tomb-helper", about = "Speedrun helper for the Tomb Raider reboot trilogy; run with no command to use it")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Get the offsets for the running game, e.g. after a patch, and save them to the offsets file next to the exe
    ///
    /// Load into a level first. It finds the overlay's offsets, Lara's position, the ground probes and the version
    /// string on its own; for the rest (e.g. cutscenes) pass the previous build's exe or a dump of it with --from. Then
    /// it checks them. It keeps a dump of the build in the data folder (`data_dir` in tomb-helper.json, else tomb-helper-data next to the exe), for the next build's --from. The offsets
    /// file it writes (tomb-helper-offsets.json) can be edited, e.g. to fix the layout, and is reloaded while
    /// tomb-helper runs.
    UpdateOffsets {
        /// The previous build's exe or a dump of it, to derive the rest of the offsets from (by default the newest dump
        /// of another known build of the game in the data folder)
        #[arg(long)]
        from: Option<String>,
        /// Which known version --from is (otherwise it's matched by image size)
        #[arg(long, alias = "version", requires = "from")]
        from_version: Option<String>,
        /// Don't save
        #[arg(long)]
        dry_run: bool,
    },
    /// Test the overlay's offsets and layout against the running game (or fit the layout, or check snapshots)
    ///
    /// With Lara somewhere ordinary in a level. --fit searches for layout offsets that make the failing checks pass:
    /// for each of their groups (or each group named, e.g. proxy) it tries moving the fields from each one on by up to
    /// 0x40 either way, and keeps what passes the most. --snapshot saves what the check read, so it can be checked again
    /// without the game with --replay, e.g. after changing the code or the offsets file.
    Check {
        #[arg(long, conflicts_with = "replay")]
        version: Option<String>,
        /// Search for layout offsets that make the checks pass, for the failing checks' groups or those named
        #[arg(long, num_args = 0.., value_name = "GROUP", conflicts_with_all = ["snapshot", "replay"])]
        fit: Option<Vec<String>>,
        /// With --fit: fit a snapshot rather than the running game
        #[arg(long, value_name = "SNAPSHOT", requires = "fit")]
        from: Option<String>,
        /// With --fit: write the result to the offsets file
        #[arg(long, requires = "fit")]
        save: bool,
        /// Save what the check read as a snapshot (in the data folder's snapshots, or to this file)
        #[arg(long, num_args = 0..=1, value_name = "FILE", conflicts_with = "replay")]
        snapshot: Option<Option<String>>,
        /// Check every snapshot (in the data folder's snapshots, or this folder) again, and show any whose results
        /// have changed since it was taken
        #[arg(long, num_args = 0..=1, value_name = "DIR")]
        replay: Option<Option<String>>,
        /// With --replay: save what each reports now as what it should report, after a change meant to change it
        #[arg(long, requires = "replay")]
        accept: bool,
    },
    /// Record the instructions that access up to 4 values (e.g. CutscenePrompt) or layout fields (e.g.
    /// proxy.in_use_size)
    ///
    /// Attaches as a debugger for a few seconds. `update-offsets --from` matches these instructions in a new build to
    /// find where an offset or a field went. --save keeps them in the version's entry in the offsets file (witnesses
    /// for values, layout_witnesses for fields); otherwise they're printed.
    Watch {
        #[arg(required = true, num_args = 1..=4, value_name = "VALUE OR GROUP.FIELD")]
        targets: Vec<String>,
        #[arg(long)]
        version: Option<String>,
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        /// Wait this many seconds first, so the game can be focused
        #[arg(long, default_value_t = 0)]
        delay: u64,
        /// Only writes (of values)
        #[arg(long)]
        write: bool,
        /// Keep the instructions in the version's entry in the offsets file next to the exe
        #[arg(long)]
        save: bool,
    },
    /// Tools for digging into the game, or into tomb-helper
    #[command(hide = true, subcommand)]
    Debug(Debug),
}

#[derive(Subcommand)]
enum Debug {
    /// Derive the offsets for <TO> using the known offsets for <FROM> (`update-offsets --from` does this too)
    ///
    /// <FROM> and <TO> are paths to an exe or a dump, or `running` for the running game.
    Derive {
        from: String,
        to: String,
        /// Which known version <FROM> is (otherwise it's matched by image size)
        #[arg(long, alias = "version")]
        from_version: Option<String>,
    },
    /// Find the overlay's static addresses through RTTI class names (`update-offsets` does this too)
    Discover {
        #[arg(long)]
        version: Option<String>,
        /// Write them to the offsets file next to the exe
        #[arg(long)]
        save: bool,
    },
    /// Write the offsets tomb-helper is using to a file (tomb-helper-offsets.json next to the exe by default)
    ExportOffsets { out: Option<String> },
    /// Save the running game's unpacked image (`update-offsets` keeps one of each build it ports)
    Dump { out: Option<String> },
    /// Find the pointer chains the game's own code uses to reach each known value
    Chains {
        #[arg(long)]
        version: Option<String>,
    },
    /// Print the current value of each known offset
    Probe {
        #[arg(long)]
        version: Option<String>,
        /// Keep printing them
        #[arg(long)]
        watch: bool,
    },
    /// Print the pointer chains the code accesses from each reference to a static address (in hex)
    Trace { image: String, address: String },
    /// Watch up to 4 addresses (absolute, or +RVA, comma separated) in any process
    WatchRaw {
        pid: String,
        addresses: String,
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        #[arg(long, default_value_t = 0)]
        delay: u64,
        /// Only writes
        #[arg(long)]
        write: bool,
        /// Break on running the instruction there, recording the registers
        #[arg(long, conflicts_with = "write")]
        exec: bool,
        /// With --exec, copy the memory a register points at, e.g. esp:40
        #[arg(long, value_parser = Dump::parse, requires = "exec")]
        dump: Option<Dump>,
    },
    /// Disassemble a running process (prints RVAs)
    Disasm {
        pid: String,
        address: String,
        #[arg(default_value = "40")]
        count: String,
    },
    /// Round trip statics between two builds to measure how well references are matched
    CheckMatching {
        a: String,
        b: String,
        #[arg(default_value_t = 200)]
        count: usize,
    },
    /// What the overlay would be sent now, without injecting it
    OverlayData {
        #[arg(long)]
        version: Option<String>,
        #[arg(long, default_value_t = 0)]
        delay: u64,
    },
}

/// Runs the command given on the command line (clap prints the help and exits for a bad one)
pub fn run() -> Result<()> {
    match Cli::parse().command {
        Command::UpdateOffsets { from, from_version, dry_run } => {
            crate::update_offsets::update_offsets(from, from_version, dry_run)
        }
        Command::Check { version, fit, from, save, snapshot, replay, accept } => {
            if let Some(groups) = fit {
                crate::fit_layout::fit_layout(&strs(&groups), from.as_deref(), version, save)
            } else if let Some(out) = snapshot {
                crate::snapshot::take(out.as_deref(), version)
            } else if let Some(dir) = replay {
                crate::snapshot::check_all(dir.as_deref(), accept)
            } else {
                crate::check::check(version)
            }
        }
        Command::Watch { targets, version, seconds, delay, write, save } => {
            let fields = targets.iter().filter(|t| t.contains('.')).count();
            if fields == targets.len() {
                if write {
                    bail!("--write is for values, not layout fields");
                }
                crate::layout_witness::watch_layout(&strs(&targets), version, seconds, delay, save)
            } else if fields == 0 {
                crate::watch::watch(&strs(&targets), version, seconds, delay, write, save)
            } else {
                bail!("Watch values (e.g. CutscenePrompt) or layout fields (e.g. proxy.in_use_size), not both at once")
            }
        }
        Command::Debug(tool) => run_debug(tool),
    }
}

fn run_debug(tool: Debug) -> Result<()> {
    match tool {
        Debug::Derive { from, to, from_version } => crate::derive::derive(&from, &to, from_version.as_deref()),
        Debug::Discover { version, save } => crate::discover::discover(version, save),
        Debug::ExportOffsets { out } => export_offsets(out.as_deref()),
        Debug::Dump { out } => dump(out.as_deref()),
        Debug::Chains { version } => crate::chains::find_code_chains(version),
        Debug::Probe { version, watch } => crate::chains::probe(version, watch),
        Debug::Trace { image, address } => crate::derive::trace(&image, &address),
        Debug::WatchRaw { pid, addresses, seconds, delay, write, exec, dump } => {
            let kind = if exec {
                Breakpoint::Execute
            } else if write {
                Breakpoint::Write
            } else {
                Breakpoint::Access
            };
            crate::watch::watch_raw(&pid, &addresses, seconds, delay, kind, dump)
        }
        Debug::Disasm { pid, address, count } => crate::watch::disasm(&pid, &address, &count),
        Debug::CheckMatching { a, b, count } => crate::derive::check_matching(&a, &b, count),
        Debug::OverlayData { version, delay } => crate::overlay_handler::print_overlay_data(version, delay),
    }
}

fn strs(strings: &[String]) -> Vec<&str> {
    strings.iter().map(String::as_str).collect()
}

fn export_offsets(out: Option<&str>) -> Result<()> {
    let path = match out {
        Some(out) => Path::new(out).to_path_buf(),
        None => OffsetsSource::new().path,
    };
    if path.exists() {
        bail!("{:?} already exists, not overwriting it", path);
    }
    let file = OffsetsFile {
        versions: known_process_details().iter().map(VersionEntry::from_details).collect(),
        words: crate::words::current().as_ref().clone(),
    };
    std::fs::write(&path, to_json(&file) + "\n").with_context(|| format!("Could not write {:?}", path))?;
    println!("Wrote {} versions to {:?}", file.versions.len(), path);
    Ok(())
}

fn dump(out: Option<&str>) -> Result<()> {
    let (image, exe) = crate::derive::running_game()?;
    let out = out.map(String::from).unwrap_or_else(|| format!("{}-{}.bin", exe.trim_end_matches(".exe"), image.size_of_image()));
    std::fs::write(&out, &image.data).with_context(|| format!("Could not write {}", out))?;
    println!("Wrote {} bytes to {}", image.data.len(), out);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Command, clap::Error> {
        Cli::try_parse_from(std::iter::once("tomb-helper").chain(args.iter().copied())).map(|cli| cli.command)
    }

    #[test]
    fn definitions_are_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn check_modes() {
        assert!(matches!(parse(&["check"]), Ok(Command::Check { fit: None, snapshot: None, replay: None, .. })));
        let fit = parse(&["check", "--fit", "proxy", "camera", "--save"]);
        assert!(matches!(fit, Ok(Command::Check { fit: Some(ref g), save: true, .. }) if g == &["proxy", "camera"]));
        assert!(matches!(parse(&["check", "--fit"]), Ok(Command::Check { fit: Some(ref g), .. }) if g.is_empty()));
        assert!(matches!(parse(&["check", "--snapshot"]), Ok(Command::Check { snapshot: Some(None), .. })));
        let replay = parse(&["check", "--replay", "--accept"]);
        assert!(matches!(replay, Ok(Command::Check { replay: Some(None), accept: true, .. })));
    }

    #[test]
    fn check_rejects_mixed_modes() {
        assert!(parse(&["check", "--fit", "--replay"]).is_err());
        assert!(parse(&["check", "--snapshot", "--replay"]).is_err());
        assert!(parse(&["check", "--accept"]).is_err());
        assert!(parse(&["check", "--save"]).is_err());
        assert!(parse(&["check", "--from", "x.snap"]).is_err());
    }

    #[test]
    fn debug_tools_are_under_debug() {
        assert!(matches!(parse(&["debug", "disasm", "1", "+10"]), Ok(Command::Debug(Debug::Disasm { .. }))));
        assert!(parse(&["disasm", "1", "+10"]).is_err());
        assert!(parse(&["fit-layout"]).is_err());
    }
}
