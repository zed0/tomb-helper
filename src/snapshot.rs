//! Snapshots of what `check` reads from a running game, to test the code reading the game's structures without the game.
//! `snapshot [<file>]` runs `check` while recording every read (`game_memory::Handle::Recording`), then saves the main
//! module, the memory read and the report `check` gives from the snapshot. `check-snapshots [<dir>]` checks each saved
//! snapshot again and reports any whose results have changed, e.g. after a refactor or a change to the offsets file.
//! Snapshots are big (the image is tens of MB), so they live in `SNAPSHOT_DIR`, not the repo.
//!
//! Scans through all of the game's memory (the trigger script scan) aren't kept, but what's read after them is, and
//! the snapshot's regions are scanned instead when it's replayed, so they find the same things.

use crate::game_memory::Handle;
use crate::offsets_file::known_process_details;
use crate::pe_image::PeImage;
use crate::process_details::ProcessDetails;
use process_memory::Pid;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where snapshots are kept by default
pub const SNAPSHOT_DIR: &str = r"H:\tomb-helper-dumps\snapshots";
const MAGIC: &[u8; 8] = b"THSNAP1\0";

/// The reads made through a recording handle, and the image if it was dumped
#[derive(Default)]
pub struct Recording {
    reads: Mutex<Vec<(usize, Vec<u8>)>>,
    image: Mutex<Option<PeImage>>,
}

impl Recording {
    pub fn add(&self, address: usize, bytes: &[u8]) {
        self.reads.lock().unwrap().push((address, bytes.to_vec()));
    }

    pub fn image(&self, pid: Pid) -> io::Result<PeImage> {
        let mut image = self.image.lock().unwrap();
        if image.is_none() {
            *image = Some(PeImage::dump(pid)?);
        }
        Ok(image.clone().unwrap())
    }

    // What was read, as non-overlapping regions in address order. Where reads overlap the first one counts (the game
    // can change between them).
    fn regions(&self) -> Vec<(usize, Vec<u8>)> {
        let reads = self.reads.lock().unwrap();
        let mut spans: Vec<(usize, usize)> = reads.iter().map(|(a, b)| (*a, a + b.len())).filter(|(a, e)| e > a).collect();
        spans.sort();
        let mut merged: Vec<(usize, usize)> = vec![];
        for (start, end) in spans {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        let mut regions: Vec<(usize, Vec<u8>)> = merged.iter().map(|(s, e)| (*s, vec![0u8; e - s])).collect();
        for (address, bytes) in reads.iter().rev().filter(|(_, b)| !b.is_empty()) {
            let i = merged.partition_point(|(start, _)| *start <= *address) - 1;
            let offset = address - regions[i].0;
            regions[i].1[offset..offset + bytes.len()].copy_from_slice(bytes);
        }
        regions
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    /// The game version's name and version string, to find its offsets
    pub game: String,
    pub version: String,
    pub base_address: usize,
    /// What `check` reported for the snapshot when it was taken
    pub report: Vec<String>,
}

pub struct Snapshot {
    pub header: Header,
    pub image: PeImage,
    // Non-overlapping, in address order
    regions: Vec<(usize, Vec<u8>)>,
}

impl Snapshot {
    pub fn copy_address(&self, address: usize, buffer: &mut [u8]) -> io::Result<()> {
        let image_start = self.header.base_address;
        if address >= image_start && address + buffer.len() <= image_start + self.image.data.len() {
            let offset = address - image_start;
            buffer.copy_from_slice(&self.image.data[offset..offset + buffer.len()]);
            return Ok(());
        }
        let i = self.regions.partition_point(|(start, _)| *start <= address);
        match i.checked_sub(1).map(|i| &self.regions[i]) {
            Some((start, bytes)) if address + buffer.len() <= start + bytes.len() => {
                buffer.copy_from_slice(&bytes[address - start..address - start + buffer.len()]);
                Ok(())
            }
            _ => Err(io::Error::new(io::ErrorKind::NotFound, format!("{:#X} isn't in the snapshot", address))),
        }
    }

    pub fn regions(&self) -> Vec<(usize, usize)> {
        self.regions.iter().map(|(start, bytes)| (*start, bytes.len())).collect()
    }

    /// The offsets file's entry for the snapshot's game version (as it is now)
    pub fn details(&self) -> Result<ProcessDetails, String> {
        known_process_details()
            .into_iter()
            .find(|d| d.name == self.header.game && d.version.version == self.header.version)
            .ok_or_else(|| format!("no offsets for {} {}", self.header.game, self.header.version))
    }

    /// Saves it with this header (its report filled in)
    pub fn save(&self, path: &Path, header: &Header) -> io::Result<()> {
        let mut out = BufWriter::new(File::create(path)?);
        out.write_all(MAGIC)?;
        write_bytes(&mut out, &serde_json::to_vec(header)?)?;
        out.write_all(&[self.image.is_64 as u8])?;
        out.write_all(&self.image.image_base.to_le_bytes())?;
        write_bytes(&mut out, &self.image.data)?;
        out.write_all(&(self.regions.len() as u64).to_le_bytes())?;
        for (start, bytes) in &self.regions {
            out.write_all(&(*start as u64).to_le_bytes())?;
            write_bytes(&mut out, bytes)?;
        }
        out.flush()
    }

    pub fn load(path: &Path) -> io::Result<Snapshot> {
        let mut input = BufReader::new(File::open(path)?);
        let mut magic = [0u8; 8];
        input.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a tomb-helper snapshot"));
        }
        let header: Header = serde_json::from_slice(&read_bytes(&mut input)?)?;
        let mut is_64 = [0u8; 1];
        input.read_exact(&mut is_64)?;
        let image_base = read_u64(&mut input)?;
        let image = PeImage { data: read_bytes(&mut input)?, is_64: is_64[0] != 0, image_base };
        let count = read_u64(&mut input)? as usize;
        let mut regions = Vec::with_capacity(count);
        for _ in 0..count {
            let start = read_u64(&mut input)? as usize;
            regions.push((start, read_bytes(&mut input)?));
        }
        Ok(Snapshot { header, image, regions })
    }
}

fn write_bytes(out: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    out.write_all(&(bytes.len() as u64).to_le_bytes())?;
    out.write_all(bytes)
}

fn read_u64(input: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0u8; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_bytes(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0u8; read_u64(input)? as usize];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// `snapshot [<file>]`: checks the running game while recording what's read, and saves that as a snapshot (in
/// `SNAPSHOT_DIR` by default)
pub fn take(out: Option<&str>, version: Option<String>) -> Result<(), String> {
    let (pid, process, base_address, details) = crate::chains::connect(version)?;
    println!("Found {} {}", details.name, details.version.version);
    let recording: &'static Recording = Box::leak(Box::default());
    let handle = Handle::Recording(pid, process, recording);
    // Failing checks are still worth a snapshot
    let _ = crate::check::run(handle, base_address, details.clone());
    let image = handle.image().map_err(|e| format!("couldn't dump the game: {}", e))?;
    let regions = recording.regions();
    let header = Header {
        game: details.name.clone(),
        version: details.version.version.clone(),
        base_address,
        report: vec![],
    };
    let snapshot: &'static Snapshot = Box::leak(Box::new(Snapshot { header, image, regions }));

    // What it reports from the snapshot is what it should go on reporting
    println!();
    println!("Checking the snapshot");
    let (lines, _) = crate::check::run_lines(Handle::Snapshot(snapshot), base_address, details);
    let header = Header { report: lines, ..snapshot.header.clone() };
    let path = match out {
        Some(path) => PathBuf::from(path),
        None => {
            std::fs::create_dir_all(SNAPSHOT_DIR).map_err(|e| e.to_string())?;
            Path::new(SNAPSHOT_DIR).join(format!("{} {}.snap", file_name(&header), timestamp()))
        }
    };
    snapshot.save(&path, &header).map_err(|e| format!("couldn't save {}: {}", path.display(), e))?;
    let size: usize = snapshot.regions.iter().map(|(_, b)| b.len()).sum();
    println!("Saved {} ({} regions, {} KB besides the image)", path.display(), snapshot.regions.len(), size / 1024);
    Ok(())
}

fn file_name(header: &Header) -> String {
    let name = format!("{} {}", header.game, header.version);
    name.chars().map(|c| if c.is_alphanumeric() || c == '.' { c } else { '_' }).collect()
}

fn timestamp() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    seconds.to_string()
}

/// Checks a snapshot again: the lines of its report that are gone (-) and new (+) since it was taken. With `accept`,
/// the new report is saved as the one to expect from now on.
pub fn replay(path: &Path, accept: bool) -> Result<Vec<String>, String> {
    let snapshot = Snapshot::load(path).map_err(|e| format!("couldn't load {}: {}", path.display(), e))?;
    // It stays in memory (handles are `Copy`, so they borrow it for good); there are only a few
    let snapshot: &'static Snapshot = Box::leak(Box::new(snapshot));
    let details = snapshot.details()?;
    let (lines, _) = crate::check::run_lines(Handle::Snapshot(snapshot), snapshot.header.base_address, details);
    let report = &snapshot.header.report;
    let gone = report.iter().filter(|line| !lines.contains(line)).map(|line| format!("- {}", line));
    let new = lines.iter().filter(|line| !report.contains(line)).map(|line| format!("+ {}", line));
    let changes: Vec<String> = gone.chain(new).collect();
    if accept && !changes.is_empty() {
        let header = Header { report: lines, ..snapshot.header.clone() };
        snapshot.save(path, &header).map_err(|e| format!("couldn't save {}: {}", path.display(), e))?;
    }
    Ok(changes)
}

/// `check-snapshots [<dir>]`: replays every snapshot in the directory (`SNAPSHOT_DIR` by default). `accept-snapshots`
/// (`accept`) also saves what each reports now as what it should report, after a change that's meant to change it.
pub fn check_all(dir: Option<&str>, accept: bool) -> Result<(), String> {
    let dir = dir.unwrap_or(SNAPSHOT_DIR);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("couldn't read {}: {}", dir, e))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map_or(false, |e| e == "snap"))
        .collect();
    paths.sort();
    if paths.is_empty() {
        return Err(format!("no snapshots in {}", dir));
    }
    let mut changed = 0;
    for path in &paths {
        let changes = replay(path, accept)?;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if changes.is_empty() {
            println!("SAME  {}", name);
        } else {
            changed += 1;
            println!("{}  {}", if accept { "SAVED" } else { "DIFF " }, name);
            for change in changes {
                println!("      {}", change);
            }
        }
    }
    if changed > 0 && accept {
        println!("Saved the new reports of {} of {} snapshots", changed, paths.len());
        return Ok(());
    }
    if changed > 0 {
        return Err(format!("{} of {} snapshots check differently (accept-snapshots saves them)", changed, paths.len()));
    }
    println!("All {} snapshots check the same", paths.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    // Needs the snapshots in `SNAPSHOT_DIR`, and sets the layout and pointer size the other tests share, so it's run on
    // its own: `cargo test --release --bin tomb-helper -- --ignored snapshots`
    #[test]
    #[ignore]
    fn snapshots_check_the_same() {
        super::check_all(None, false).unwrap();
    }
}
