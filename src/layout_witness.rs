//! Layout witnesses: instructions in the game that read or write a field of its structures (`layout`), found with
//! `watch-layout` and kept in the version's `layout_witnesses`, matched into a new build (`update-offsets --from`) to
//! see where the field went, the way `witnesses` do for the offsets file's last offsets. Fields are named
//! `group.field` as in the offsets file's layout section, e.g. `proxy.in_use_size`.
//!
//! `group_address` finds a field's structure in the running game (camera, proxy, instance, collision scene, instance
//! and mesh so far); the instructions accessing it are recorded with its offset as their displacement
//! (`watch::record`), and `derive_layout` matches them into a new build (`witness`). Fields are found by name through
//! the layout's JSON (`field_offset`, `with_field`).

use anyhow::{anyhow, bail, Context, Result};
use crate::components::Components;
use crate::discover;
use crate::find_process;
use crate::game::Game;
use crate::game_memory::{read_pointer, Handle};
use crate::layout::Layout;
use crate::object_list::ObjectList;
use crate::offsets_file::{to_json, Hex, VersionEntry};
use crate::pe_image::PeImage;
use crate::process_details::{AddressType, ProcessDetails};
use crate::watch::{self, Breakpoint, Hit};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

// Witnesses kept per field
const MAX_WITNESSES: usize = 8;
// Of the witnesses that match, at least this share have to agree on where the field went
const MIN_AGREEMENT: f64 = 0.5;

/// A field's offset in the layout, if it's set
pub fn field_offset(layout: &Layout, name: &str) -> Option<usize> {
    let (group, field) = name.split_once('.')?;
    let value = serde_json::to_value(layout).ok()?;
    let text = value.get(group)?.get(field)?.as_str()?;
    usize::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

/// The layout with one field changed
pub fn with_field(layout: &Layout, name: &str, offset: usize) -> Result<Layout> {
    let (group, field) = name.split_once('.').with_context(|| format!("{} isn't group.field", name))?;
    let mut value = serde_json::to_value(layout)?;
    let slot = value
        .get_mut(group)
        .and_then(|g| g.get_mut(field))
        .with_context(|| format!("the layout has no {}", name))?;
    *slot = serde_json::Value::String(format!("{:#X}", offset));
    Ok(serde_json::from_value(value)?)
}

// Where a group's structure is in the running game, for the ones there's a way to find
fn group_address(game: &Game, components: &mut Components, group: &str) -> Result<usize> {
    let layout = crate::layout::current();
    let missing = |what: &str| format!("{} wasn't found", what);
    match group {
        // GameRender: what the camera's chains go through
        "camera" => {
            let chain = game.offsets.get(&AddressType::CameraView).with_context(|| missing("CameraView"))?;
            crate::game_memory::resolve(game.handle, game.base_address, &[chain[0], 0]).with_context(|| missing("GameRender"))
        }
        "proxy" => {
            let lara = lara_instance(game, components);
            game.lara_proxy(components, lara).with_context(|| missing("Lara's proxy"))
        }
        "instance" => lara_instance(game, components).with_context(|| missing("Lara's instance")),
        "collision_scene" => game.resolve(AddressType::CollisionScene).with_context(|| missing("CollisionScene")),
        "collision_instance" => {
            let scene = game.resolve(AddressType::CollisionScene).with_context(|| missing("CollisionScene"))?;
            read_pointer(game.handle, scene + layout.collision_scene.first).with_context(|| missing("a collision instance"))
        }
        "collision_mesh" => {
            let instance = group_address(game, components, "collision_instance")?;
            read_pointer(game.handle, instance + layout.collision_instance.mesh).with_context(|| missing("a collision mesh"))
        }
        other => Err(anyhow!("there's no way to find a {} structure in the game yet (see group_address)", other)),
    }
}

// Lara's instance: her static proxy's (TR2013), or the one with a PlayerComponent nearest her position
fn lara_instance(game: &Game, components: &mut Components) -> Option<usize> {
    let layout = crate::layout::current();
    if let (Some(proxy), Some(offset)) = (game.resolve(AddressType::PlayerProxy), layout.proxy.instance) {
        return read_pointer(game.handle, proxy + offset);
    }
    let position = game.resolve(AddressType::XPosition)?;
    let position = crate::game_memory::read_floats::<3>(game.handle, position)?;
    let instances = ObjectList::new(game.resolve(AddressType::InstanceArray)?).instances(game.handle).ok()?;
    let mut nearest: Vec<(f32, usize)> = instances
        .iter()
        .map(|i| {
            let d = (i.position.x - position[0]).powi(2) + (i.position.y - position[1]).powi(2) + (i.position.z - position[2]).powi(2);
            (d, i.address)
        })
        .collect();
    nearest.sort_by(|a, b| a.0.total_cmp(&b.0));
    let addresses: Vec<usize> = nearest.into_iter().map(|(_, a)| a).collect();
    components.allow_scans(addresses.len().min(64));
    game.find_lara(components, &addresses)
}

/// `watch-layout <group.field>... [--seconds <n>] [--delay <n>] [--save]`: records the instructions that access up to 4
/// layout fields in the running game, and with `save` keeps them as the version's layout witnesses
pub fn watch_layout(names: &[&str], version: Option<String>, seconds: u64, delay: u64, save: bool) -> Result<()> {
    if names.is_empty() || names.len() > watch::MAX_WATCHES {
        bail!("Give between 1 and {} layout fields (group.field)", watch::MAX_WATCHES);
    }
    let (pid, handle, base_address, details) = crate::chains::connect(version)?;
    println!("Found {} {}", details.name, details.version.version);
    let game = Game::new(Handle::Live(pid, handle), base_address, &details.address_offsets);
    let mut components = Components::new(base_address);
    let mut fields = vec![];
    for name in names {
        let offset = field_offset(&details.layout, name).with_context(|| format!("The layout has no {} set", name))?;
        // The instructions using it have no displacement to match in another build
        if offset == 0 {
            bail!("{} is at the start of its structure, so instructions can't show where it went", name);
        }
        let group = name.split_once('.').map_or("", |(g, _)| g);
        let address = group_address(&game, &mut components, group)? + offset;
        println!("{} at {:#X} ({:#X} into the structure)", name, address, offset);
        fields.push((name.to_string(), offset, address));
    }
    if delay > 0 {
        println!("Starting in {} seconds...", delay);
        std::thread::sleep(Duration::from_secs(delay));
    }
    let addresses: Vec<usize> = fields.iter().map(|(_, _, a)| *a).collect();
    let image_size = find_process::get_image_size(handle, base_address)?;
    let code = Some((base_address, base_address + image_size));
    let hits = watch::record(pid, handle, base_address, &addresses, seconds, Breakpoint::Access, code, None)?;

    let mut witnesses: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for ((name, offset, _), hits) in fields.iter().zip(&hits) {
        // Only instructions using the field's own offset tell where it went (others go through another pointer)
        let mut best: Vec<(&usize, &Hit)> = hits.iter().filter(|(_, hit)| hit.displacement == *offset as i64).collect();
        best.sort_by_key(|(rva, hit)| (std::cmp::Reverse(hit.count), **rva));
        best.truncate(MAX_WITNESSES);
        println!("{}: {} instructions accessed it, {} with its offset", name, hits.len(), best.len());
        for (rva, hit) in &best {
            println!("    +{:#X} {:>6}x  {}", rva, hit.count, hit.text);
        }
        if !best.is_empty() {
            witnesses.insert(name.clone(), best.iter().map(|(rva, _)| **rva).collect());
        }
    }
    let as_hex: BTreeMap<String, Vec<Hex>> =
        witnesses.iter().map(|(n, w)| (n.clone(), w.iter().map(|r| Hex(*r)).collect())).collect();
    println!("\n\"layout_witnesses\": {}", to_json(&as_hex));
    if save && !witnesses.is_empty() {
        let mut entry = VersionEntry::from_details(&details);
        entry.layout_witnesses.extend(as_hex);
        discover::save_entry(entry)?;
    }
    Ok(())
}

/// A layout field `derive_layout` worked out for a new build
pub struct LayoutChange {
    pub name: String,
    pub old: usize,
    // Where the witnesses that matched agree it is, if they do
    pub new: Option<usize>,
    pub matched: usize,
    pub witnesses: usize,
}

/// Where each field with witnesses in `details` (whose image is `old`) is in `new`
pub fn derive_layout(old: &PeImage, details: &ProcessDetails, new: &PeImage) -> Vec<LayoutChange> {
    let mut changes = vec![];
    let mut names: Vec<&String> = details.layout_witnesses.keys().collect();
    names.sort();
    for name in names {
        let rvas = &details.layout_witnesses[name];
        let old_offset = match field_offset(&details.layout, name) {
            Some(offset) => offset,
            None => continue,
        };
        let matches: Vec<i64> = crate::witness::match_witnesses(old, new, rvas)
            .into_iter()
            .flatten()
            .map(|m| old_offset as i64 + m.new_displacement - m.old_displacement)
            .collect();
        let mut votes: HashMap<i64, usize> = HashMap::new();
        for offset in &matches {
            *votes.entry(*offset).or_insert(0) += 1;
        }
        let best = votes.iter().max_by_key(|(offset, count)| (**count, -**offset)).map(|(o, c)| (*o, *c));
        let new_offset = best
            .filter(|(offset, count)| *offset >= 0 && *count as f64 >= matches.len() as f64 * MIN_AGREEMENT)
            .map(|(offset, _)| offset as usize);
        changes.push(LayoutChange { name: name.clone(), old: old_offset, new: new_offset, matched: matches.len(), witnesses: rvas.len() });
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_found_and_changed_by_name() {
        let layout = Layout::default();
        assert_eq!(field_offset(&layout, "proxy.in_use_size"), Some(layout.proxy.in_use_size));
        assert_eq!(field_offset(&layout, "proxy.nothing"), None);
        // Unset optional fields have no offset
        assert_eq!(field_offset(&layout, "proxy.instance"), None);
        let changed = with_field(&layout, "proxy.in_use_size", 0x99).unwrap();
        assert_eq!(changed.proxy.in_use_size, 0x99);
        assert_eq!(changed.proxy.centre, layout.proxy.centre);
        assert_eq!(with_field(&layout, "proxy.instance", 0x18).unwrap().proxy.instance, Some(0x18));
    }
}
