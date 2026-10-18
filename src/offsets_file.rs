//! The per-version offsets, stored as JSON. `offsets.json` in the repo is built into the exe, and a
//! `tomb-helper-offsets.json` next to the exe replaces it, so offsets can be edited without
//! rebuilding (and are reloaded while tomb-helper is running).

use crate::layout::Layout;
use crate::process_details::{AddressType, ProcessDetails, VersionIdentifier};
use process_memory::Architecture;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::SystemTime;
use std::{env, fs};

const BUILT_IN: &str = include_str!("../offsets.json");
pub const FILE_NAME: &str = "tomb-helper-offsets.json";

/// A number written as a hex string ("0x3621B00", underscores allowed), or a plain number
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hex(pub usize);

impl Serialize for Hex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{:#X}", self.0))
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Hex, D::Error> {
        struct HexVisitor;
        impl<'de> Visitor<'de> for HexVisitor {
            type Value = Hex;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a hex string like \"0x3621B00\" or a number")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Hex, E> {
                Ok(Hex(value as usize))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Hex, E> {
                let digits = value
                    .strip_prefix("0x")
                    .or_else(|| value.strip_prefix("0X"))
                    .ok_or_else(|| E::custom(format!("\"{}\" should start with 0x", value)))?
                    .replace('_', "");
                usize::from_str_radix(&digits, 16)
                    .map(Hex)
                    .map_err(|_| E::custom(format!("\"{}\" isn't a hex number", value)))
            }
        }
        deserializer.deserialize_any(HexVisitor)
    }
}

fn to_hex(values: &[usize]) -> Vec<Hex> {
    values.iter().map(|v| Hex(*v)).collect()
}

fn from_hex(values: &[Hex]) -> Vec<usize> {
    values.iter().map(|v| v.0).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OffsetsFile {
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionEntry {
    pub game: String,
    pub executable: String,
    // The version string in memory, which is read using the `Version` offsets
    pub version: String,
    // Size of the image in memory, used instead of the version string when set (e.g. to tell Steam
    // and Epic builds apart, since they share version strings)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_size: Option<usize>,
    pub bits: u32,
    // Pointer chains in the same form as `TrackedMemory`: the first is relative to the module base
    pub offsets: BTreeMap<AddressType, Vec<Hex>>,
    // RVAs of instructions that access each value (`tomb-helper watch`), only used by `derive`
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub witnesses: BTreeMap<AddressType, Vec<Hex>>,
    // Offsets a tool found but couldn't confirm, for review. Not used.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unverified: BTreeMap<AddressType, Vec<Hex>>,
    // Where things are in the game's structures, where they differ from ROTTR (Steam)'s (see `layout`)
    #[serde(default, skip_serializing_if = "Layout::is_default")]
    pub layout: Layout,
}

impl VersionEntry {
    pub fn from_details(details: &ProcessDetails) -> VersionEntry {
        VersionEntry {
            game: details.name.clone(),
            executable: details.executable_name.clone(),
            version: details.version.version.clone(),
            image_size: details.version.image_size,
            bits: if matches!(details.arch, Architecture::Arch64Bit) { 64 } else { 32 },
            offsets: details.address_offsets.iter().map(|(t, o)| (*t, to_hex(o))).collect(),
            witnesses: details.witnesses.iter().map(|(t, w)| (*t, to_hex(w))).collect(),
            unverified: BTreeMap::new(),
            layout: details.layout.clone(),
        }
    }

    fn to_details(&self) -> Result<ProcessDetails, String> {
        let arch = match self.bits {
            64 => Architecture::Arch64Bit,
            32 => Architecture::Arch32Bit,
            other => return Err(format!("{}: bits should be 32 or 64, not {}", self.version, other)),
        };
        if let Some((address_type, _)) = self.offsets.iter().find(|(_, o)| o.is_empty()) {
            return Err(format!("{}: {:?} has no offsets", self.version, address_type));
        }
        Ok(ProcessDetails {
            name: self.game.clone(),
            executable_name: self.executable.clone(),
            version: VersionIdentifier {
                version: self.version.clone(),
                image_size: self.image_size,
            },
            arch,
            address_offsets: self.offsets.iter().map(|(t, o)| (*t, from_hex(o))).collect(),
            witnesses: self.witnesses.iter().map(|(t, w)| (*t, from_hex(w))).collect(),
            layout: self.layout.clone(),
        })
    }
}

pub fn parse(json: &str) -> Result<Vec<ProcessDetails>, String> {
    let file: OffsetsFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
    file.versions.iter().map(VersionEntry::to_details).collect()
}

/// Pretty JSON, but with arrays of plain values kept on one line so each offset list is one line
pub fn to_json<T: Serialize>(value: &T) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap();
    let mut out = String::new();
    let mut rest = pretty.as_str();
    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        let end = rest[start..].find(']').map(|e| start + e);
        match end {
            Some(end) if !rest[start + 1..end].contains(|c| c == '{' || c == '[') => {
                let items: Vec<&str> = rest[start + 1..end]
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                out.push('[');
                out.push_str(&items.join(", "));
                out.push(']');
                rest = &rest[end + 1..];
            }
            _ => {
                out.push('[');
                rest = &rest[start + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The offsets file tomb-helper uses: the one next to the exe if there is one, otherwise the
/// built in one. Remembers when it was loaded so changes can be picked up.
pub struct OffsetsSource {
    pub path: PathBuf,
    modified: Option<SystemTime>,
}

fn modified(path: &PathBuf) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl OffsetsSource {
    pub fn new() -> OffsetsSource {
        OffsetsSource {
            path: env::current_exe().unwrap().parent().unwrap().join(FILE_NAME),
            modified: None,
        }
    }

    pub fn load(&mut self) -> Result<Vec<ProcessDetails>, String> {
        self.modified = modified(&self.path);
        if self.modified.is_none() {
            return parse(BUILT_IN).map_err(|e| format!("Built in offsets are invalid: {}", e));
        }
        let json = fs::read_to_string(&self.path).map_err(|e| format!("Could not read {:?}: {}", self.path, e))?;
        parse(&json).map_err(|e| format!("Could not load {:?}: {}", self.path, e))
    }

    pub fn describe(&self) -> String {
        if self.modified.is_some() {
            format!("{:?}", self.path)
        } else {
            format!("built in offsets (create {:?} to override them)", self.path)
        }
    }

    /// Whether the file has been created, changed or removed since it was last loaded
    pub fn changed(&self) -> bool {
        modified(&self.path) != self.modified
    }
}

/// The known versions, for the command line tools. Exits on an invalid offsets file.
pub fn known_process_details() -> Vec<ProcessDetails> {
    OffsetsSource::new().load().unwrap_or_else(|e| {
        eprintln!("{}", e);
        std::process::exit(1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_offsets_are_valid() {
        let versions = parse(BUILT_IN).unwrap();
        assert!(!versions.is_empty());
    }

    #[test]
    fn parses_hex_strings_and_numbers() {
        let json = r#"{"versions": [{
            "game": "Game", "executable": "Game.exe", "version": "v1", "bits": 64,
            "offsets": {"XPosition": ["0x0141_E298", "0x0", 16]}
        }]}"#;
        let versions = parse(json).unwrap();
        assert_eq!(versions[0].address_offsets[&AddressType::XPosition], vec![0x0141_E298, 0x0, 0x10]);
    }

    #[test]
    fn rejects_typos() {
        let json = r#"{"versions": [{
            "game": "Game", "executable": "Game.exe", "version": "v1", "bits": 64,
            "offset": {"XPosition": ["0x10"]}
        }]}"#;
        assert!(parse(json).is_err());
        let json = r#"{"versions": [{
            "game": "Game", "executable": "Game.exe", "version": "v1", "bits": 64,
            "offsets": {"XPositon": ["0x10"]}
        }]}"#;
        assert!(parse(json).is_err());
    }

    #[test]
    fn reads_layout_overrides() {
        let json = r#"{"versions": [{
            "game": "Game", "executable": "Game.exe", "version": "v1", "bits": 64,
            "offsets": {"XPosition": ["0x10"]},
            "layout": {"instance": {"position": "0x20"}}
        }]}"#;
        let versions = parse(json).unwrap();
        assert_eq!(versions[0].layout.instance.position, 0x20);
        assert_eq!(versions[0].layout.instance.rotation, Layout::default().instance.rotation);
    }

    #[test]
    fn keeps_offset_lists_on_one_line() {
        let mut offsets = BTreeMap::new();
        offsets.insert(AddressType::XPosition, vec![Hex(0x10), Hex(0x20)]);
        let json = to_json(&offsets);
        assert!(json.contains(r#""XPosition": ["0x10", "0x20"]"#), "{}", json);
    }
}

