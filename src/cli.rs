//! The command line tools (`tomb-helper <command>`); with no command tomb-helper runs normally (`main`).

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
    /// it checks them.
    UpdateOffsets {
        /// The previous build's exe or a dump of it, to derive the rest of the offsets from
        #[arg(long)]
        from: Option<String>,
        /// Which known version --from is (otherwise it's matched by image size)
        #[arg(long, alias = "version", requires = "from")]
        from_version: Option<String>,
        /// Don't save
        #[arg(long)]
        dry_run: bool,
    },
    /// Write the offsets tomb-helper is using to a file to edit
    ///
    /// By default this is tomb-helper-offsets.json next to the exe, which tomb-helper then uses instead of its built
    /// in offsets, and reloads whenever it changes.
    ExportOffsets { out: Option<String> },
    /// Save the running game's unpacked image for later use
    Dump { out: Option<String> },
    /// Derive the offsets for <TO> using the known offsets for <FROM>
    ///
    /// <FROM> and <TO> are paths to an exe or a dump, or `running` for the running game.
    Derive {
        from: String,
        to: String,
        /// Which known version <FROM> is (otherwise it's matched by image size)
        #[arg(long, alias = "version")]
        from_version: Option<String>,
    },
    /// Find pointer chains that the game's own code uses to reach each known value
    ///
    /// With the game running and loaded into a level. These can be derived for new versions much more reliably than
    /// pointer scanned chains.
    Chains {
        #[arg(long)]
        version: Option<String>,
    },
    /// Print the current value of each known offset for the running game
    Probe {
        #[arg(long)]
        version: Option<String>,
        /// Keep printing them
        #[arg(long)]
        watch: bool,
    },
    /// Find the overlay's static addresses through the RTTI class names of what they point to
    ///
    /// With the game running and loaded into a level: the camera, instance array and collision scene, which don't
    /// change between versions.
    Discover {
        #[arg(long)]
        version: Option<String>,
        /// Write them to the offsets file next to the exe
        #[arg(long)]
        save: bool,
    },
    /// Test the overlay's offsets and layout against the running game
    ///
    /// With Lara somewhere ordinary in a level, e.g. after `discover` or editing the layout for a new version.
    Check {
        #[arg(long)]
        version: Option<String>,
    },
    /// Record the instructions that access up to 4 layout fields (e.g. proxy.in_use_size)
    ///
    /// Like `watch`, so `update-offsets --from` can find where the fields went in a new build.
    WatchLayout {
        #[arg(required = true, num_args = 1..=4, value_name = "GROUP.FIELD")]
        fields: Vec<String>,
        #[arg(long)]
        version: Option<String>,
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        /// Wait this many seconds first, so the game can be focused
        #[arg(long, default_value_t = 0)]
        delay: u64,
        /// Keep them as the version's layout_witnesses in the offsets file next to the exe
        #[arg(long)]
        save: bool,
    },
    /// Search for layout offsets that make `check` pass
    ///
    /// For each group of a failing check (or each group named, e.g. proxy), try moving its fields from each one on by
    /// up to 0x40 either way, and keep what passes the most. On the running game, or a snapshot.
    FitLayout {
        groups: Vec<String>,
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long)]
        version: Option<String>,
        /// Write the result to the offsets file
        #[arg(long)]
        save: bool,
    },
    /// Run `check` on the running game and save what it read as a snapshot
    ///
    /// To H:/tomb-helper-dumps/snapshots by default, so it can be checked again without the game.
    Snapshot {
        out: Option<String>,
        #[arg(long)]
        version: Option<String>,
    },
    /// Check every snapshot again, and show any whose results have changed since it was taken
    CheckSnapshots { dir: Option<String> },
    /// The same, saving what each reports now as what it should report (after a change meant to change it)
    AcceptSnapshots { dir: Option<String> },
    /// Print the pointer chains the code accesses from each reference to a static address
    Trace {
        image: String,
        /// In hex
        address: String,
    },
    /// Record which instructions access up to 4 values (e.g. CutscenePrompt)
    ///
    /// Attaches as a debugger for a few seconds. These let `derive` find where a field moved to.
    Watch {
        #[arg(required = true, num_args = 1..=4, value_name = "ADDRESS TYPE")]
        names: Vec<String>,
        #[arg(long)]
        version: Option<String>,
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        /// Wait this many seconds first, so the game can be focused
        #[arg(long, default_value_t = 0)]
        delay: u64,
        /// Only writes
        #[arg(long)]
        write: bool,
    },
    /// Watch up to 4 addresses (absolute, or +RVA) in any process
    #[command(hide = true)]
    WatchRaw {
        pid: String,
        /// Comma separated
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
    #[command(hide = true)]
    Disasm {
        pid: String,
        address: String,
        #[arg(default_value = "40")]
        count: String,
    },
    /// Round trip statics between two builds to measure how well they're matched
    #[command(hide = true)]
    CheckMatching {
        a: String,
        b: String,
        #[arg(default_value_t = 200)]
        count: usize,
    },
    /// What the overlay would be sent now, without injecting it
    #[command(hide = true)]
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
        Command::ExportOffsets { out } => export_offsets(out.as_deref()),
        Command::Dump { out } => dump(out.as_deref()),
        Command::Derive { from, to, from_version } => crate::derive::derive(&from, &to, from_version.as_deref()),
        Command::Chains { version } => crate::chains::find_code_chains(version),
        Command::Probe { version, watch } => crate::chains::probe(version, watch),
        Command::Discover { version, save } => crate::discover::discover(version, save),
        Command::Check { version } => crate::check::check(version),
        Command::WatchLayout { fields, version, seconds, delay, save } => {
            crate::layout_witness::watch_layout(&strs(&fields), version, seconds, delay, save)
        }
        Command::FitLayout { groups, snapshot, version, save } => {
            crate::fit_layout::fit_layout(&strs(&groups), snapshot.as_deref(), version, save)
        }
        Command::Snapshot { out, version } => crate::snapshot::take(out.as_deref(), version),
        Command::CheckSnapshots { dir } => crate::snapshot::check_all(dir.as_deref(), false),
        Command::AcceptSnapshots { dir } => crate::snapshot::check_all(dir.as_deref(), true),
        Command::Trace { image, address } => crate::derive::trace(&image, &address),
        Command::Watch { names, version, seconds, delay, write } => {
            crate::watch::watch(&strs(&names), version, seconds, delay, write)
        }
        Command::WatchRaw { pid, addresses, seconds, delay, write, exec, dump } => {
            let kind = if exec {
                Breakpoint::Execute
            } else if write {
                Breakpoint::Write
            } else {
                Breakpoint::Access
            };
            crate::watch::watch_raw(&pid, &addresses, seconds, delay, kind, dump)
        }
        Command::Disasm { pid, address, count } => crate::watch::disasm(&pid, &address, &count),
        Command::CheckMatching { a, b, count } => crate::derive::check_matching(&a, &b, count),
        Command::OverlayData { version, delay } => crate::overlay_handler::print_overlay_data(version, delay),
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
