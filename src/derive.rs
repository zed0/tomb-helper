//! Derives the offsets for a new game version from a version we already know.
//!
//! Static addresses: each known static address is referenced from code (RIP-relative on 64-bit,
//! absolute on 32-bit). The same functions exist in the new build, but byte-exact signatures don't
//! survive patches (different instruction encodings, inverted branches, moved calls). So instead
//! every reference in the new build with the same opcode + ModRM is scored by how many of the
//! surrounding bytes match, and the best unambiguous match votes for where the address now lives.
//! References to nearby globals (the same struct) vote too, offset by their distance from the target.
//!
//! Pointer chains: the code at each pair of matched references is traced in both builds (see
//! `code_trace`), the accesses are aligned, and each aligned pair votes for how an old chain maps to
//! a new one. Chains are then mapped one level at a time. A level the code never touches directly is
//! shifted by the same amount as its nearest neighbouring field.

use crate::chains;
use crate::code_trace::{Access, Tracer};
use crate::find_process;
use crate::offsets_file::{to_json, Hex, OffsetsFile, OffsetsSource, VersionEntry};
use crate::pe_image::PeImage;
use crate::witness;
use crate::offsets_file::known_process_details;
use crate::process_details::{AddressType, ProcessDetails};
use process_memory::Architecture;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

// Bytes compared either side of the 4-byte address field
const WINDOW: usize = 32;
// Fraction of window bytes that must match, and how far the best match must beat the runner-up
const MIN_SCORE: f64 = 0.6;
const MIN_MARGIN: f64 = 0.1;
// For pairing references to an already known address, which needs less evidence
const MIN_PAIR_SCORE: f64 = 0.5;
const MAX_REFERENCES: usize = 60;
// Try references to the target's own struct first, then widen (e.g. TR2013 reads position as a
// field 0x220 into a global struct, so nothing references it directly)
const SPANS: [usize; 2] = [0x40, 0x400];

const USAGE: &str = "\
Usage:
  tomb-helper                         Run normally
  tomb-helper export-offsets [<out.json>]
      Write the offsets tomb-helper is using to a file to edit. By default this is
      tomb-helper-offsets.json next to the exe, which tomb-helper then uses instead of its built in
      offsets, and reloads whenever it changes.
  tomb-helper dump [<out.bin>]        Save the running game's unpacked image for later use
  tomb-helper derive <from> <to> [--from-version <version>]
      Derive the offsets for <to> using the known offsets for <from>.
      <from>/<to> are paths to an exe or a dump, or `running` for the running game.
      --from-version picks the known version for <from>; otherwise it's matched by image size.
  tomb-helper chains [--version <version>]
      With the game running and loaded into a level, find pointer chains that the game's own code
      uses to reach each known value. These can be derived for new versions much more reliably
      than pointer scanned chains.
  tomb-helper probe [--version <version>] [--watch]
      Print the current value of each known offset for the running game.
  tomb-helper trace <image> <static address in hex>
      Print the pointer chains the code accesses from each reference to a static address.
  tomb-helper watch <address type>... [--version <version>] [--seconds <n>] [--delay <n>]
      Attach as a debugger and record which instructions access up to 4 values (e.g. CutscenePrompt)
      for a few seconds (default 5), optionally after a delay so the game can be focused.
      These let `derive` find where a field moved to.";

pub fn run(args: &[String]) -> Result<(), String> {
    let mut positional = vec![];
    let mut version = None;
    let mut watch = false;
    let mut seconds = 5;
    let mut delay = 0;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--from-version" | "--version" => {
                version = Some(rest.next().ok_or(format!("{} needs a value", arg))?.clone())
            }
            "--watch" => watch = true,
            "--delay" => delay = rest.next().and_then(|s| s.parse().ok()).ok_or("--delay needs a number")?,
            "--seconds" => {
                seconds = rest
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--seconds needs a number")?
            }
            _ => positional.push(arg.as_str()),
        }
    }
    match positional.as_slice() {
        ["watch-raw", pid, address] => crate::watch::watch_raw(pid, address, seconds),
        ["watch", names @ ..] => crate::watch::watch(names, version, seconds, delay),
        ["export-offsets"] => export_offsets(None),
        ["export-offsets", out] => export_offsets(Some(out)),
        ["dump"] => dump(None),
        ["dump", out] => dump(Some(out)),
        ["derive", from, to] => derive(from, to, version.as_deref()),
        ["chains"] => chains::find_code_chains(version),
        ["probe"] => chains::probe(version, watch),
        ["trace", image, address] => trace(image, address),
        _ => Err(USAGE.to_string()),
    }
}

fn export_offsets(out: Option<&str>) -> Result<(), String> {
    let path = match out {
        Some(out) => Path::new(out).to_path_buf(),
        None => OffsetsSource::new().path,
    };
    if path.exists() {
        return Err(format!("{:?} already exists, not overwriting it", path));
    }
    let file = OffsetsFile {
        versions: known_process_details().iter().map(VersionEntry::from_details).collect(),
    };
    std::fs::write(&path, to_json(&file) + "
").map_err(|e| format!("Could not write {:?}: {}", path, e))?;
    println!("Wrote {} versions to {:?}", file.versions.len(), path);
    Ok(())
}

fn running_game() -> Result<(PeImage, String), String> {
    let mut executables: Vec<String> = known_process_details()
        .into_iter()
        .map(|d| d.executable_name)
        .collect();
    executables.dedup();
    for exe in executables {
        if let Some(pid) = find_process::get_pid(&exe) {
            println!("Dumping {} (PID {})...", exe, pid);
            let image = PeImage::dump(pid).map_err(|e| format!("Could not dump {}: {}", exe, e))?;
            return Ok((image, exe));
        }
    }
    Err("No known game is running".to_string())
}

fn load(source: &str) -> Result<PeImage, String> {
    if source == "running" {
        return running_game().map(|(image, _)| image);
    }
    println!("Loading {}...", source);
    PeImage::load(Path::new(source)).map_err(|e| format!("Could not load {}: {}", source, e))
}

fn dump(out: Option<&str>) -> Result<(), String> {
    let (image, exe) = running_game()?;
    let out = out.map(String::from).unwrap_or_else(|| {
        format!("{}-{}.bin", exe.trim_end_matches(".exe"), image.size_of_image())
    });
    std::fs::write(&out, &image.data).map_err(|e| format!("Could not write {}: {}", out, e))?;
    println!("Wrote {} bytes to {}", image.data.len(), out);
    Ok(())
}

fn trace(image: &str, address: &str) -> Result<(), String> {
    let image = load(image)?;
    let address = usize::from_str_radix(address.trim_start_matches("0x"), 16).map_err(|e| e.to_string())?;
    let references = References::new(&image);
    let tracer = Tracer::new(&image);
    for &(target, position) in references.between(address, address) {
        match tracer.instruction_referencing(position as usize, target as usize) {
            Some(start) => {
                let chains: Vec<String> = tracer
                    .trace(start)
                    .iter()
                    .map(|a| {
                        let chain: Vec<usize> = a.chain.iter().map(|o| *o as usize).collect();
                        format!("{} {:?}", format_offsets(&chain), a.mnemonic)
                    })
                    .collect();
                println!("{:#X}: {}", start, chains.join(" | "));
            }
            None => println!("{:#X}: (no instruction found)", position),
        }
    }
    Ok(())
}

fn reference_details(image: &PeImage, from_version: Option<&str>) -> Result<ProcessDetails, String> {
    let known = known_process_details();
    let found = match from_version {
        Some(version) => known.iter().find(|d| d.version.version == version),
        None => known
            .iter()
            .find(|d| d.version.image_size == Some(image.size_of_image())),
    };
    let details = found.cloned().ok_or_else(|| {
        let versions: Vec<String> = known
            .iter()
            .map(|d| format!("  {} \"{}\"", d.name, d.version.version))
            .collect();
        format!(
            "Could not identify the <from> version (image size {}). Pass --from-version with one of:\n{}",
            image.size_of_image(),
            versions.join("\n")
        )
    })?;
    if matches!(details.arch, Architecture::Arch64Bit) != image.is_64 {
        return Err(format!("{} doesn't match the architecture of <from>", details.version.version));
    }
    Ok(details)
}

/// The static address referred to by the 4-byte address field at `position`, if it is one
pub fn reference_target(image: &PeImage, position: usize) -> Option<usize> {
    let d = &image.data;
    let value = u32::from_le_bytes([d[position], d[position + 1], d[position + 2], d[position + 3]]);
    let target = if image.is_64 {
        // ModRM with mod=00 rm=101 is [rip+disp32]
        if d[position - 1] & 0xC7 != 0x05 {
            return None;
        }
        (position as i64 + 4 + value as i32 as i64) as usize
    } else {
        (value as u64).wrapping_sub(image.image_base) as usize
    };
    if target >= 0x1000 && target < d.len() {
        Some(target)
    } else {
        None
    }
}

// The two bytes before the address field, i.e. opcode + ModRM
fn key(data: &[u8], position: usize) -> usize {
    (data[position - 2] as usize) << 8 | data[position - 1] as usize
}

/// Every reference to a static address in an image, indexed by target and by key
pub struct References<'a> {
    pub image: &'a PeImage,
    // (target, position), sorted
    by_target: Vec<(u32, u32)>,
    // positions bucketed by key
    by_key: Vec<Vec<u32>>,
}

impl<'a> References<'a> {
    pub fn new(image: &'a PeImage) -> References<'a> {
        let end = image.data.len() - WINDOW - 4;
        let mut by_target: Vec<(u32, u32)> = (WINDOW + 2..end)
            .into_par_iter()
            .filter_map(|position| reference_target(image, position).map(|t| (t as u32, position as u32)))
            .collect();
        by_target.par_sort_unstable();
        let mut by_key = vec![vec![]; 0x10000];
        for (_, position) in &by_target {
            by_key[key(&image.data, *position as usize)].push(*position);
        }
        References {
            image,
            by_target,
            by_key,
        }
    }

    /// (target, position) of every reference to an address in [low, high]
    pub fn between(&self, low: usize, high: usize) -> &[(u32, u32)] {
        let start = self.by_target.partition_point(|(t, _)| (*t as usize) < low);
        let end = self.by_target.partition_point(|(t, _)| (*t as usize) <= high);
        &self.by_target[start..end]
    }

    pub fn all(&self) -> &[(u32, u32)] {
        &self.by_target
    }
}

fn similarity(a: &[u8], p: usize, b: &[u8], q: usize) -> usize {
    let before = a[p - WINDOW..p].iter().zip(&b[q - WINDOW..q]).filter(|(x, y)| x == y).count();
    let after = a[p + 4..p + 4 + WINDOW]
        .iter()
        .zip(&b[q + 4..q + 4 + WINDOW])
        .filter(|(x, y)| x == y)
        .count();
    before + after
}

fn fraction(score: usize) -> f64 {
    score as f64 / (2 * WINDOW) as f64
}

/// A reference in the old build and its best match in the new build
#[derive(Debug, Clone, Copy)]
struct Pair {
    old_position: usize,
    old_target: usize,
    new_position: usize,
    new_target: usize,
    score: f64,
}

struct Derivation {
    references: usize,
    matched: usize,
    // Candidate addresses in the new build, best first
    votes: Vec<(usize, f64)>,
    pairs: Vec<Pair>,
}

impl Derivation {
    fn best(&self) -> Option<usize> {
        self.votes.first().map(|(address, _)| *address)
    }

    fn confident(&self) -> bool {
        let total: f64 = self.votes.iter().map(|(_, v)| v).sum();
        self.matched >= 2 && self.votes.first().map_or(false, |(_, v)| *v >= 0.8 * total)
    }
}

fn derive_address(old: &References, new: &References, target: usize) -> Derivation {
    let mut result = Derivation {
        references: 0,
        matched: 0,
        votes: vec![],
        pairs: vec![],
    };
    for span in SPANS.iter() {
        let mut nearby: Vec<(u32, u32)> = old
            .between(target.saturating_sub(*span), target + span)
            .to_vec();
        nearby.sort_by_key(|(t, p)| ((*t as i64 - target as i64).abs(), *p));
        nearby.truncate(MAX_REFERENCES);

        let matches: Vec<Pair> = nearby
            .par_iter()
            .filter_map(|&(old_target, position)| {
                let position = position as usize;
                let mut best = (0, 0);
                let mut second = 0;
                for &q in &new.by_key[key(&old.image.data, position)] {
                    let score = similarity(&old.image.data, position, &new.image.data, q as usize);
                    if score > best.0 {
                        second = best.0;
                        best = (score, q as usize);
                    } else if score > second {
                        second = score;
                    }
                }
                if fraction(best.0) < MIN_SCORE || fraction(best.0 - second) < MIN_MARGIN {
                    return None;
                }
                reference_target(new.image, best.1).map(|new_target| Pair {
                    old_position: position,
                    old_target: old_target as usize,
                    new_position: best.1,
                    new_target,
                    score: fraction(best.0),
                })
            })
            .collect();

        let mut votes: HashMap<usize, f64> = HashMap::new();
        for pair in &matches {
            let delta = pair.old_target as i64 - target as i64;
            let weight = if delta == 0 { 1.0 } else { 0.5 };
            *votes.entry((pair.new_target as i64 - delta) as usize).or_default() += weight * pair.score;
        }
        let mut votes: Vec<(usize, f64)> = votes.into_iter().collect();
        votes.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        result = Derivation {
            references: nearby.len(),
            matched: matches.len(),
            votes,
            pairs: matches,
        };
        if result.matched > 0 {
            break;
        }
    }

    // Keep only the pairs that agree with the winner, then pair up any remaining references to the
    // target itself with the most similar reference to the derived address.
    if let Some(new_base) = result.best() {
        result
            .pairs
            .retain(|p| p.new_target as i64 - p.old_target as i64 == new_base as i64 - target as i64);
        let paired: Vec<usize> = result.pairs.iter().map(|p| p.old_position).collect();
        let candidates = new.between(new_base, new_base);
        let extra: Vec<Pair> = old
            .between(target, target)
            .par_iter()
            .filter(|(_, p)| !paired.contains(&(*p as usize)))
            .filter_map(|&(_, p)| {
                candidates
                    .iter()
                    .map(|&(_, q)| (similarity(&old.image.data, p as usize, &new.image.data, q as usize), q))
                    .max()
                    .filter(|(score, _)| fraction(*score) >= MIN_PAIR_SCORE)
                    .map(|(score, q)| Pair {
                        old_position: p as usize,
                        old_target: target,
                        new_position: q as usize,
                        new_target: new_base,
                        score: fraction(score),
                    })
            })
            .collect();
        result.pairs.extend(extra);
    }
    result
}

/// Longest common subsequence of two access lists, matching accesses of the same shape
fn align<'a>(old: &'a [Access], new: &'a [Access]) -> Vec<(&'a Access, &'a Access)> {
    let (n, m) = (old.len(), new.len());
    let mut table = vec![vec![0_u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if old[i].shape() == new[j].shape() {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut aligned = vec![];
    while i < n && j < m {
        if old[i].shape() == new[j].shape() && table[i][j] == table[i + 1][j + 1] + 1 {
            aligned.push((&old[i], &new[j]));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    aligned
}

/// Old chain -> new chain -> votes
type ChainVotes = HashMap<Vec<i64>, HashMap<Vec<i64>, f64>>;

fn chain_votes(old: &PeImage, new: &PeImage, pairs: &[Pair]) -> ChainVotes {
    let old_tracer = Tracer::new(old);
    let new_tracer = Tracer::new(new);
    let per_pair: Vec<Vec<(Vec<i64>, Vec<i64>, f64)>> = pairs
        .par_iter()
        .filter_map(|pair| {
            let old_start = old_tracer.instruction_referencing(pair.old_position, pair.old_target)?;
            let new_start = new_tracer.instruction_referencing(pair.new_position, pair.new_target)?;
            let old_accesses = old_tracer.trace(old_start);
            let new_accesses = new_tracer.trace(new_start);
            Some(
                align(&old_accesses, &new_accesses)
                    .into_iter()
                    .map(|(a, b)| (a.chain.clone(), b.chain.clone(), pair.score))
                    .collect(),
            )
        })
        .collect();

    let mut votes: ChainVotes = HashMap::new();
    for (old_chain, new_chain, weight) in per_pair.into_iter().flatten() {
        *votes.entry(old_chain).or_default().entry(new_chain).or_default() += weight;
    }
    votes
}

#[derive(Debug, Clone, PartialEq)]
enum LevelSource {
    // Matched accesses in code, with their total vote
    Code(f64),
    // Moved by the same amount as a neighbouring field the code does use
    Shifted { neighbour: i64, shift: i64 },
    // No evidence, kept as it was
    Copied,
    // The last level, from matching instructions seen accessing the value (`tomb-helper watch`).
    // If the code alignment says otherwise, that's kept too as a warning.
    Witness { votes: f64, code_disagrees: Option<i64> },
}

/// The new last offset suggested by witness instructions, with its vote
type WitnessVote = Option<(i64, f64)>;

fn map_chain(
    chain: &[usize],
    new_base: usize,
    votes: &ChainVotes,
    witness: WitnessVote,
) -> (Vec<usize>, Vec<LevelSource>) {
    let chain: Vec<i64> = chain.iter().map(|o| *o as i64).collect();
    let mut mapped: Vec<i64> = vec![new_base as i64];
    let mut sources = vec![];

    for level in 1..chain.len() {
        let mut direct: HashMap<i64, f64> = HashMap::new();
        // (old offset, new offset) of sibling fields at this level -> votes
        let mut siblings: HashMap<(i64, i64), f64> = HashMap::new();
        for (old_chain, news) in votes {
            if old_chain.len() <= level || old_chain[..level] != chain[..level] {
                continue;
            }
            for (new_chain, weight) in news {
                if new_chain.len() != old_chain.len() || new_chain[..level] != mapped[..] {
                    continue;
                }
                if old_chain[level] == chain[level] {
                    *direct.entry(new_chain[level]).or_default() += weight;
                }
                *siblings.entry((old_chain[level], new_chain[level])).or_default() += weight;
            }
        }

        let code = direct.into_iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

        // Instructions seen accessing the value itself are the best evidence for the last level
        if let (true, Some((offset, weight))) = (level == chain.len() - 1, witness) {
            mapped.push(offset);
            sources.push(LevelSource::Witness {
                votes: weight,
                code_disagrees: code.map(|(o, _)| o).filter(|o| *o != offset),
            });
            continue;
        }

        if let Some((offset, weight)) = code {
            mapped.push(offset);
            sources.push(LevelSource::Code(weight));
            continue;
        }

        // Use the shift of the closest field below (fields after an insertion move together),
        // or above if there is none below
        let wanted = chain[level];
        let mut by_neighbour: BTreeMap<i64, HashMap<i64, f64>> = BTreeMap::new();
        for ((old_offset, new_offset), weight) in siblings {
            *by_neighbour
                .entry(old_offset)
                .or_default()
                .entry(new_offset - old_offset)
                .or_default() += weight;
        }
        let neighbour = by_neighbour
            .range(..=wanted)
            .next_back()
            .or_else(|| by_neighbour.range(wanted..).next());
        match neighbour {
            Some((neighbour, shifts)) => {
                let shift = *shifts
                    .iter()
                    .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                    .unwrap()
                    .0;
                mapped.push(wanted + shift);
                sources.push(LevelSource::Shifted {
                    neighbour: *neighbour,
                    shift,
                });
            }
            None => {
                mapped.push(wanted);
                sources.push(LevelSource::Copied);
            }
        }
    }
    (mapped.into_iter().map(|o| o as usize).collect(), sources)
}

fn format_base(address: usize) -> String {
    let hex = format!("{:08X}", address);
    let groups: Vec<&str> = (0..hex.len())
        .step_by(2)
        .map(|i| &hex[i..i + 2])
        .collect();
    format!("0x{}", groups.join("_"))
}

pub fn format_offsets(chain: &[usize]) -> String {
    let mut parts = vec![format_base(chain[0])];
    parts.extend(chain[1..].iter().map(|o| format!("{:#X}", o)));
    format!("vec![{}]", parts.join(", "))
}

fn describe(sources: &[LevelSource]) -> String {
    sources
        .iter()
        .map(|s| match s {
            LevelSource::Code(votes) => format!("code ({:.1})", votes),
            LevelSource::Shifted { neighbour, shift } => format!(
                "shifted {}{:#X} like {:#X}",
                if *shift < 0 { "-" } else { "+" },
                shift.unsigned_abs(),
                neighbour
            ),
            LevelSource::Copied => "copied".to_string(),
            LevelSource::Witness { votes, code_disagrees } => match code_disagrees {
                Some(code) => format!("witness ({:.1}) but code says {:#X}", votes, code),
                None => format!("witness ({:.1})", votes),
            },
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Prints a version entry for the offsets file. Offsets that aren't `sure` go under `unverified`,
/// which tomb-helper ignores, so they can be checked before being moved into `offsets`.
pub fn print_entry(
    details: &ProcessDetails,
    image: &PeImage,
    version: &str,
    lines: &[(AddressType, Option<Vec<usize>>, bool, String)],
    witnesses: &HashMap<AddressType, Vec<usize>>,
) {
    for (address_type, chain, sure, note) in lines {
        println!(
            "//   {:<17} {}{}",
            format!("{:?}", address_type),
            match (chain, sure) {
                (None, _) => "[NOT FOUND] ",
                (Some(_), false) => "[UNSURE] ",
                _ => "",
            },
            note
        );
    }
    let hex = |chain: &Vec<usize>| chain.iter().map(|o| Hex(*o)).collect::<Vec<Hex>>();
    let entry = VersionEntry {
        game: details.name.clone(),
        executable: details.executable_name.clone(),
        version: version.to_string(),
        image_size: Some(image.size_of_image()),
        bits: if image.is_64 { 64 } else { 32 },
        offsets: lines
            .iter()
            .filter_map(|(t, chain, sure, _)| chain.as_ref().filter(|_| *sure).map(|c| (*t, hex(c))))
            .collect(),
        witnesses: witnesses.iter().map(|(t, w)| (*t, hex(w))).collect(),
        unverified: lines
            .iter()
            .filter_map(|(t, chain, sure, _)| chain.as_ref().filter(|_| !*sure).map(|c| (*t, hex(c))))
            .collect(),
    };
    println!("{}", to_json(&entry));
}

fn derive(from: &str, to: &str, from_version: Option<&str>) -> Result<(), String> {
    let old = load(from)?;
    // A running game can be identified the same way the main tool does, including by version string
    let running = match (from, from_version) {
        ("running", None) => find_process::find_process(known_process_details(), None).map(|(_, _, _, d)| d),
        _ => None,
    };
    let details = match running {
        Some(details) => details,
        None => reference_details(&old, from_version)?,
    };
    let new = load(to)?;
    if old.is_64 != new.is_64 {
        return Err("<from> and <to> have different architectures".to_string());
    }

    println!("Indexing references...");
    let old_refs = References::new(&old);
    let new_refs = References::new(&new);

    // Several address types usually share a static address (e.g. X/Y/Z position)
    let mut by_base: BTreeMap<usize, Vec<AddressType>> = BTreeMap::new();
    for (address_type, offsets) in &details.address_offsets {
        by_base.entry(offsets[0]).or_default().push(*address_type);
    }

    println!("\nStatic addresses derived from {} {}:", details.name, details.version.version);
    let mut derived: HashMap<usize, (Derivation, ChainVotes)> = HashMap::new();
    for (base, types) in &by_base {
        let derivation = derive_address(&old_refs, &new_refs, *base);
        let candidates: Vec<String> = derivation
            .votes
            .iter()
            .take(3)
            .map(|(address, votes)| format!("{:#X} ({:.1})", address, votes))
            .collect();
        println!(
            "  {:#010X} {:<60} {:>2}/{:<2} refs matched -> {} {}",
            base,
            format!("{:?}", types),
            derivation.matched,
            derivation.references,
            if candidates.is_empty() { "none".to_string() } else { candidates.join(", ") },
            if derivation.best().is_none() || derivation.confident() { "" } else { "[UNSURE]" },
        );
        let votes = chain_votes(&old, &new, &derivation.pairs);
        derived.insert(*base, (derivation, votes));
    }

    let mut entries: Vec<(&AddressType, &Vec<usize>)> = details.address_offsets.iter().collect();
    entries.sort_by_key(|(address_type, _)| **address_type);

    // Each witness that matches votes for the last offset moving by as much as its displacement did
    let mut witness_votes: HashMap<AddressType, WitnessVote> = HashMap::new();
    for (address_type, rvas) in &details.witnesses {
        let last = match details.address_offsets.get(address_type) {
            Some(offsets) => *offsets.last().unwrap() as i64,
            None => continue,
        };
        let matches = witness::match_witnesses(&old, &new, rvas);
        let mut votes: HashMap<i64, f64> = HashMap::new();
        for m in matches.iter().flatten() {
            *votes.entry(last + m.new_displacement - m.old_displacement).or_default() += m.score;
        }
        println!(
            "  {:?}: {}/{} witnesses matched -> {}",
            address_type,
            matches.iter().flatten().count(),
            rvas.len(),
            votes
                .iter()
                .map(|(o, v)| format!("{:#X} ({:.1})", o, v))
                .collect::<Vec<_>>()
                .join(", ")
        );
        witness_votes.insert(
            *address_type,
            votes.into_iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap()),
        );
    }

    let lines: Vec<(AddressType, Option<Vec<usize>>, bool, String)> = entries
        .into_iter()
        .map(|(address_type, offsets)| {
            let (derivation, votes) = &derived[&offsets[0]];
            match derivation.best() {
                Some(new_base) => {
                    let witness = witness_votes.get(address_type).cloned().flatten();
                    let (chain, sources) = map_chain(offsets, new_base, votes, witness);
                    let sure = derivation.confident()
                        && !sources.iter().any(|s| {
                            matches!(
                                s,
                                LevelSource::Copied
                                    | LevelSource::Witness {
                                        code_disagrees: Some(_),
                                        ..
                                    }
                            )
                        });
                    (*address_type, Some(chain), sure, describe(&sources))
                }
                None => (*address_type, None, false, String::new()),
            }
        })
        .collect();

    println!(
        "\n// Derived from {} {}. Levels marked `code` were matched in the game's code, `shifted`\n\
         // levels moved like a neighbouring field, `witness` levels came from matching instructions\n\
         // seen accessing the value, and `copied` levels had no evidence.\n\
         // Add this to the offsets file, fill in the version string, and check the values in game\n\
         // with `tomb-helper probe`.",
        details.name, details.version.version
    );
    print_entry(&details, &new, "TODO", &lines, &HashMap::new());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::Mnemonic;

    fn access(chain: &[i64], loads_pointer: bool) -> Access {
        Access {
            chain: chain.to_vec(),
            mnemonic: Mnemonic::Mov,
            size: 8,
            loads_pointer,
            instruction: 0,
        }
    }

    #[test]
    fn formats_offsets_like_the_table() {
        assert_eq!(format_offsets(&[0x0362_1B00, 0xD8, 0x0B70]), "vec![0x03_62_1B_00, 0xD8, 0xB70]");
        assert_eq!(format_offsets(&[0x00A7_88F0]), "vec![0x00_A7_88_F0]");
    }

    #[test]
    fn similarity_ignores_the_address_field() {
        let a: Vec<u8> = (0..100).collect();
        let mut b = a.clone();
        b[50..54].copy_from_slice(&[0xFF; 4]);
        assert_eq!(similarity(&a, 50, &b, 50), 2 * WINDOW);
    }

    #[test]
    fn aligns_accesses_of_the_same_shape() {
        let old = vec![access(&[1], true), access(&[1, 0xC60], true), access(&[1, 0xC60, 0x60], false)];
        let new = vec![access(&[2], true), access(&[2, 0x8], false), access(&[2, 0xD70], true), access(&[2, 0xD70, 0x60], false)];
        let aligned: Vec<(Vec<i64>, Vec<i64>)> = align(&old, &new)
            .into_iter()
            .map(|(a, b)| (a.chain.clone(), b.chain.clone()))
            .collect();
        assert_eq!(
            aligned,
            vec![(vec![1], vec![2]), (vec![1, 0xC60], vec![2, 0xD70]), (vec![1, 0xC60, 0x60], vec![2, 0xD70, 0x60])]
        );
    }

    #[test]
    fn maps_chains_level_by_level_with_shift_fallback() {
        let mut votes: ChainVotes = HashMap::new();
        votes.entry(vec![1, 0xC60]).or_default().insert(vec![2, 0xD70], 3.0);
        votes.entry(vec![1, 0xC60, 0x200]).or_default().insert(vec![2, 0xD70, 0x200], 1.0);
        votes.entry(vec![1, 0xC50]).or_default().insert(vec![2, 0xD60], 2.0);

        let (chain, sources) = map_chain(&[1, 0xC60, 0x200], 2, &votes, None);
        assert_eq!(chain, vec![2, 0xD70, 0x200]);
        assert_eq!(sources, vec![LevelSource::Code(4.0), LevelSource::Code(1.0)]);

        // 0xC58 is never accessed directly, but the field below it moved by 0x110
        let (chain, sources) = map_chain(&[1, 0xC58], 2, &votes, None);
        assert_eq!(chain, vec![2, 0xD68]);
        assert_eq!(sources, vec![LevelSource::Shifted { neighbour: 0xC50, shift: 0x110 }]);

        // A witness wins for the last level, but a disagreeing code match is reported
        let (chain, sources) = map_chain(&[1, 0xC60, 0x200], 2, &votes, Some((0x210, 2.0)));
        assert_eq!(chain, vec![2, 0xD70, 0x210]);
        assert_eq!(sources[1], LevelSource::Witness { votes: 2.0, code_disagrees: Some(0x200) });

        // Nothing is known below 0xC60, so the last level is kept
        let (chain, sources) = map_chain(&[1, 0xC60, 0x60, 0x8], 2, &votes, None);
        assert_eq!(chain, vec![2, 0xD70, 0x60 + 0x0, 0x8]);
        assert_eq!(sources[2], LevelSource::Copied);
    }
}
