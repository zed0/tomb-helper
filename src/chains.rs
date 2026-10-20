//! Live tools that need the game running: finding the pointer chains the game's own code uses to
//! reach each known value, and printing the current values.

use anyhow::{bail, Context, Result};
use crate::code_trace::{Access, CallResolver, Tracer};
use crate::derive::{format_offsets, print_entry, References};
use crate::find_process;
use crate::pe_image::PeImage;
use crate::offsets_file::known_process_details;
use crate::process_details::{AddressType, ProcessDetails};
use process_memory::{Architecture, CopyAddress, Pid, ProcessHandle};
use rayon::prelude::*;
use std::collections::HashMap;
use std::{thread, time};

const CANDIDATES_SHOWN: usize = 5;

pub fn connect(version: Option<String>) -> Result<(Pid, ProcessHandle, usize, ProcessDetails)> {
    let connected = find_process::find_process(known_process_details(), version)
        .context("No known game version is running (pass --version to force one from the offsets file)")?;
    crate::layout::set_for(&connected.3);
    Ok(connected)
}

fn read_pointer(handle: ProcessHandle, address: usize, pointer_size: usize) -> Option<usize> {
    let mut bytes = [0_u8; 8];
    handle
        .copy_address(address, &mut bytes[..pointer_size])
        .ok()
        .map(|_| u64::from_le_bytes(bytes) as usize)
}

// Process handles can be used from any thread, but are raw pointers so aren't `Sync` by default
struct SharedHandle(ProcessHandle);
unsafe impl Sync for SharedHandle {}
unsafe impl Send for SharedHandle {}

struct Memory {
    handle: ProcessHandle,
    base_addr: usize,
    pointer_size: usize,
    cache: HashMap<usize, Option<usize>>,
}

impl Memory {
    fn new(handle: ProcessHandle, base_addr: usize, arch: Architecture) -> Memory {
        Memory {
            handle,
            base_addr,
            pointer_size: crate::game_memory::pointer_size_of(arch),
            cache: HashMap::new(),
        }
    }

    fn read_pointer(&mut self, address: usize) -> Option<usize> {
        if let Some(value) = self.cache.get(&address) {
            return *value;
        }
        let value = read_pointer(self.handle, address, self.pointer_size);
        self.cache.insert(address, value);
        value
    }

    fn resolve(&mut self, chain: &[i64]) -> Option<usize> {
        resolve(chain, self.base_addr, |address| self.read_pointer(address))
    }
}

/// The address a chain points to, following the same rules as `TrackedMemory`
fn resolve(chain: &[i64], base_addr: usize, mut read: impl FnMut(usize) -> Option<usize>) -> Option<usize> {
    let mut address = (base_addr as i64 + chain[0]) as usize;
    for offset in &chain[1..] {
        let pointer = read(address)?;
        if pointer < 0x10000 {
            return None;
        }
        address = (pointer as i64 + offset) as usize;
    }
    Some(address)
}

fn to_chain(offsets: &[usize]) -> Vec<i64> {
    offsets.iter().map(|o| *o as i64).collect()
}

struct Stats {
    sites: u32,
    access: Access,
}

#[derive(Debug, Clone)]
struct Candidate {
    chain: Vec<i64>,
    // The code accesses exactly this address, rather than a wider value containing it
    exact: bool,
    sites: u32,
    how: String,
}

/// Every chain that some code in the image accesses, with the number of places that access it
fn code_chains(image: &PeImage, resolver: CallResolver) -> HashMap<Vec<i64>, Stats> {
    let references = References::new(image);
    let tracer = Tracer::new(image).with_resolver(resolver);
    let mut starts: Vec<usize> = references
        .all()
        .par_iter()
        .filter_map(|&(target, position)| tracer.instruction_referencing(position as usize, target as usize))
        .collect();
    starts.par_sort_unstable();
    starts.dedup();
    println!("Tracing {} instructions that use static addresses...", starts.len());

    starts
        .par_iter()
        .fold(HashMap::new, |mut stats: HashMap<Vec<i64>, Stats>, &start| {
            let mut accesses = tracer.trace(start);
            accesses.sort_by(|a, b| a.chain.cmp(&b.chain));
            accesses.dedup_by(|a, b| a.chain == b.chain);
            for access in accesses {
                stats
                    .entry(access.chain.clone())
                    .and_modify(|s| s.sites += 1)
                    .or_insert(Stats { sites: 1, access });
            }
            stats
        })
        .reduce(HashMap::new, |mut a, b| {
            for (chain, s) in b {
                a.entry(chain)
                    .and_modify(|existing| existing.sites += s.sites)
                    .or_insert(s);
            }
            a
        })
}

/// For each target address, the chains the game's code uses to reach it, best first: exact accesses, then chains used
/// in several places (so they can be matched in other versions), then shorter chains
fn candidate_chains(
    image: &PeImage,
    handle: ProcessHandle,
    base_addr: usize,
    arch: Architecture,
    targets: &[(AddressType, usize)],
) -> HashMap<AddressType, Vec<Candidate>> {
    let shared = SharedHandle(handle);
    let mut memory = Memory::new(handle, base_addr, arch);
    let pointer_size = memory.pointer_size;
    let image_size = image.size_of_image();
    let resolver = move |chain: &[i64]| -> Option<usize> {
        let slot = resolve(chain, base_addr, |a| read_pointer(shared.0, a, pointer_size))?;
        let function = read_pointer(shared.0, slot, pointer_size)?;
        function.checked_sub(base_addr).filter(|rva| *rva < image_size)
    };
    let chains = code_chains(&image, &resolver);
    println!("Evaluating {} code chains...", chains.len());

    let mut candidates: HashMap<AddressType, HashMap<Vec<i64>, Candidate>> = HashMap::new();
    let mut add = |t: AddressType, candidate: Candidate| {
        if candidate.chain[1..].iter().any(|o| *o < 0) {
            return;
        }
        let existing = candidates.entry(t).or_default();
        let keep = existing
            .get(&candidate.chain)
            .map_or(true, |c| (c.exact, c.sites) < (candidate.exact, candidate.sites));
        if keep {
            existing.insert(candidate.chain.clone(), candidate);
        }
    };
    for (chain, stats) in &chains {
        let address = match memory.resolve(chain) {
            Some(a) => a,
            None => continue,
        };
        let size = stats.access.size.max(1);
        let pointer = if stats.access.loads_pointer {
            memory.read_pointer(address)
        } else {
            None
        };
        for &(t, target) in targets {
            if address <= target && target < address + size {
                let mut found = chain.clone();
                *found.last_mut().unwrap() += (target - address) as i64;
                add(
                    t,
                    Candidate {
                        chain: found,
                        exact: address == target,
                        sites: stats.sites,
                        how: format!("{:?} {} bytes", stats.access.mnemonic, size),
                    },
                );
            }
            if pointer == Some(target) {
                let mut found = chain.clone();
                found.push(0);
                add(
                    t,
                    Candidate {
                        chain: found,
                        exact: true,
                        sites: stats.sites,
                        how: "pointer to it loaded by code".to_string(),
                    },
                );
            }
        }
    }

    candidates
        .into_iter()
        .map(|(t, found)| {
            let mut found: Vec<Candidate> = found.into_values().collect();
            found.sort_by_key(|c| (!c.exact, c.sites < 2, c.chain.len(), std::cmp::Reverse(c.sites)));
            (t, found)
        })
        .collect()
}

/// The best chain the game's code uses to reach each target address, with how many places use it
pub fn best_code_chains(
    image: &PeImage,
    handle: ProcessHandle,
    base_addr: usize,
    arch: Architecture,
    targets: &[(AddressType, usize)],
) -> HashMap<AddressType, (Vec<usize>, u32)> {
    candidate_chains(image, handle, base_addr, arch, targets)
        .into_iter()
        .filter_map(|(t, found)| {
            let best = found.into_iter().next()?;
            Some((t, (best.chain.iter().map(|o| *o as usize).collect(), best.sites)))
        })
        .collect()
}

pub fn find_code_chains(version: Option<String>) -> Result<()> {
    let (pid, handle, base_addr, details) = connect(version)?;
    println!("Found {} {}", details.name, details.version.version);
    let mut memory = Memory::new(handle, base_addr, details.arch);

    let mut entries: Vec<(AddressType, Vec<usize>)> = details
        .address_offsets
        .iter()
        .map(|(t, o)| (*t, o.clone()))
        .collect();
    entries.sort_by_key(|(t, _)| *t);

    let targets: Vec<(AddressType, Option<usize>)> = entries
        .iter()
        .map(|(t, offsets)| (*t, memory.resolve(&to_chain(offsets))))
        .collect();
    for (t, address) in &targets {
        if address.is_none() {
            println!("  {:?}: current chain doesn't resolve, skipping", t);
        }
    }
    if targets.iter().all(|(_, a)| a.is_none()) {
        bail!("None of the current chains resolve. Load into a level and try again.");
    }

    println!("Dumping the game...");
    let image = PeImage::dump(pid).context("Could not dump the game")?;
    let known: Vec<(AddressType, usize)> = targets.iter().filter_map(|(t, a)| a.map(|a| (*t, a))).collect();
    let mut candidates = candidate_chains(&image, handle, base_addr, details.arch, &known);

    let mut lines = vec![];
    println!();
    for ((t, offsets), (_, target)) in entries.iter().zip(&targets) {
        println!("{:?}: current {} -> {}", t, format_offsets(offsets), target.map_or("unresolved".to_string(), |a| format!("{:#X}", a)));
        let found: Vec<Candidate> = candidates.remove(t).unwrap_or_default();
        if found.is_empty() {
            println!("    no code chains found");
        }
        for c in found.iter().take(CANDIDATES_SHOWN) {
            let chain: Vec<usize> = c.chain.iter().map(|o| *o as usize).collect();
            println!(
                "    {:<50} {:>4} sites  {}{}",
                format_offsets(&chain),
                c.sites,
                c.how,
                if c.exact { "" } else { " (inside a wider access)" }
            );
        }
        lines.push(match found.first() {
            Some(c) => (
                *t,
                Some(c.chain.iter().map(|o| *o as usize).collect()),
                true,
                format!("code chain, {} sites", c.sites),
            ),
            None => (*t, Some(offsets.clone()), true, "no code chain found, kept current".to_string()),
        });
    }

    println!(
        "\n// Code chains for {} {}, to replace its entry in the offsets file. Check them with\n\
         // `tomb-helper probe` after restarting the game.",
        details.name, details.version.version
    );
    print_entry(&details, &image, &details.version.version, &lines, &details.witnesses);
    Ok(())
}

fn describe_value(handle: ProcessHandle, address: usize, address_type: AddressType, version: &str) -> String {
    if address_type == AddressType::Version {
        let mut bytes = vec![0_u8; version.len().max(32)];
        return match handle.copy_address(address, &mut bytes) {
            Ok(_) => {
                let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
                format!("\"{}\"", String::from_utf8_lossy(&bytes[..end]))
            }
            Err(_) => "unreadable".to_string(),
        };
    }
    let mut bytes = [0_u8; 4];
    match handle.copy_address(address, &mut bytes) {
        Ok(_) => format!(
            "f32 {:<14} i32 {:<12} hex {:08X}",
            f32::from_le_bytes(bytes),
            i32::from_le_bytes(bytes),
            u32::from_le_bytes(bytes)
        ),
        Err(_) => "unreadable".to_string(),
    }
}

/// Prints the current value of each of the offsets in `details`
pub fn print_values(handle: ProcessHandle, base_addr: usize, details: &ProcessDetails) {
    let mut entries: Vec<(&AddressType, &Vec<usize>)> = details.address_offsets.iter().collect();
    entries.sort_by_key(|(t, _)| **t);
    let mut memory = Memory::new(handle, base_addr, details.arch);
    for (t, offsets) in &entries {
        let value = match memory.resolve(&to_chain(offsets)) {
            Some(address) => describe_value(handle, address, **t, &details.version.version),
            None => "chain doesn't resolve".to_string(),
        };
        println!("  {:<17} {:<45} {}", format!("{:?}", t), format_offsets(offsets), value);
    }
}

pub fn probe(version: Option<String>, watch: bool) -> Result<()> {
    let (pid, handle, base_addr, details) = connect(version)?;
    println!("Found {} {}", details.name, details.version.version);
    loop {
        print_values(handle, base_addr, &details);
        if !watch || !find_process::is_process_running(pid) {
            return Ok(());
        }
        println!();
        thread::sleep(time::Duration::from_millis(500));
    }
}
