//! `debug watch <group.field>...`: the instructions that access up to 4 layout fields (named `group.field` as in the
//! offsets file's layout section, e.g. `proxy.in_use_size`) in the running game, found with hardware breakpoints
//! (`watch::record`). `group_address` finds a field's structure (camera, proxy, instance, collision scene, instance and
//! mesh so far).

use anyhow::{anyhow, bail, Context, Result};
use crate::components::Components;
use crate::find_process;
use crate::game::Game;
use crate::game_memory::{read_pointer, Handle};
use crate::layout::field_offset;
use crate::object_list::ObjectList;
use crate::process_details::AddressType;
use crate::watch::{self, Breakpoint, Hit};
use std::time::Duration;

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

/// `debug watch <group.field>... [--seconds <n>] [--delay <n>]`
pub fn watch_fields(names: &[&str], version: Option<String>, seconds: u64, delay: u64) -> Result<()> {
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
    for ((name, offset, _), hits) in fields.iter().zip(&hits) {
        let mut by_count: Vec<(&usize, &Hit)> = hits.iter().collect();
        by_count.sort_by_key(|(rva, hit)| (std::cmp::Reverse(hit.count), **rva));
        let own = by_count.iter().filter(|(_, hit)| hit.displacement == *offset as i64).count();
        println!("{}: {} instructions accessed it, {} with its offset", name, by_count.len(), own);
        for (rva, hit) in &by_count {
            println!("    +{:#X} {:>6}x  {}", rva, hit.count, hit.text);
        }
    }
    Ok(())
}
