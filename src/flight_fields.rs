//! Finding the layout fields flight mode writes through, the same way in every game, from the running game's structures
//! and code (read only; the fall start and velocity need her lifted, see `fall_start`):
//! - her proxy's collision bit and resolved centre: with her standing, the resolved centre is the proxy's other exact
//!   copy of her centre, and the proxy's code tests the bit in its flags, then writes it (TR2013 copies the centre or the
//!   collision result into +0x60, ROTTR its centre into +0x70);
//! - the camera's collision switch: GameRender -> `CameraComponent` -> `ExplorationCameraController` (directly, or
//!   through one more pointer as SOTTR's camera stack has it), then the byte before its `cdc::GeomSphere` (inline, or
//!   pointed to) that's set and that the code compares (three passes in every game);
//! - the body list's previous link: the offset near `body.next` that leads back from each body's next.
//! `debug flight-fields [--save]`, and `port`.

use anyhow::{Context, Result};
use crate::game::Game;
use crate::game_memory::{pointer_size, read_floats, read_pointer, read_u8};
use crate::layout::Layout;
use crate::pe_image::PeImage;
use crate::process_details::AddressType;
use crate::rtti::Classes;
use iced_x86::{Decoder, DecoderOptions, Instruction, MemorySize, Mnemonic, OpKind, Register};
use process_memory::CopyAddress;
use std::collections::{BTreeMap, BTreeSet};

const CAMERA_COMPONENT: &str = ".?AVCameraComponent@@";
const CONTROLLER: &str = crate::rtti::EXPLORATION_CAMERA;
const SPHERE: &str = ".?AVGeomSphere@cdc@@";
// How far into GameRender, the camera component, a stack entry and the controller to look
const RENDER_SPAN: usize = 0x1000;
const CAMERA_SPAN: usize = 0x2000;
const ENTRY_SPAN: usize = 0x40;
const CONTROLLER_SPAN: usize = 0x2000;
// The switch is just before the sphere: 0x10 in ROTTR and TR2013, 0x18 in SOTTR
const SWITCH_BEFORE_SPHERE: usize = 0x40;
// After the collision bit's test, how many instructions to look through for the store
const STORE_WINDOW: usize = 24;
// How close two copies of her centre have to be to be the same
const SAME_CENTRE: f32 = 0.01;
// How far either side of `body.next` the previous link can be, and how many bodies along to check it
const PREVIOUS_NEAR: usize = 0x18;
const PREVIOUS_CHECKS: usize = 64;

/// What was found, each None where it wasn't
#[derive(Debug, Default)]
pub struct Found {
    /// The bit of the proxy's flags set while it collides, and the resolved centre's offset
    pub collision: Option<(usize, usize)>,
    pub camera_collision: Option<Vec<usize>>,
    pub previous: Option<usize>,
    /// What was seen along the way, for the report
    pub notes: Vec<String>,
}

impl Found {
    /// The layout with what was found in it
    pub fn apply(&self, mut layout: Layout) -> Layout {
        if let Some((bit, resolved)) = self.collision {
            layout.proxy.movement_collides = Some(bit);
            layout.proxy.resolved_centre = Some(resolved);
        }
        if let Some(path) = &self.camera_collision {
            layout.camera.collision = path.clone();
        }
        if let Some(previous) = self.previous {
            layout.body.previous = Some(previous);
        }
        layout
    }
}

/// Finds them in the running game, whose image is `image`
pub fn find(game: &Game, image: &PeImage) -> Found {
    let layout = crate::layout::current();
    let mut found = Found::default();
    let mut classes = Classes::new(game.base_address);
    let mut components = crate::components::Components::new(game.base_address);
    components.allow_scans(usize::MAX);
    let lara = crate::fall_start::lara_instance(game);
    let proxy = game.lara_proxy(&mut components, lara);

    // The camera's path to its controller and the switch candidates, which the code scan counts compares of
    let camera = find_controller(game, &mut classes);
    let switches: Vec<usize> = match &camera {
        Some((_, controller, sphere)) => (sphere.saturating_sub(SWITCH_BEFORE_SPHERE)..*sphere)
            .filter(|&o| read_u8(game.handle, controller + o) == Some(1))
            .collect(),
        None => {
            found.notes.push("camera: no ExplorationCameraController found from GameRender".to_string());
            vec![]
        }
    };
    let flags = layout.proxy.flags;
    let scan = scan_code(image, flags, &switches);

    // The collision bit and resolved centre
    match (proxy, flags) {
        (Some(proxy), Some(_)) => {
            let centre = read_vector(game, proxy + layout.proxy.centre);
            let copies: BTreeMap<(usize, usize), usize> = scan
                .stores_after_tests
                .iter()
                .filter(|((_, offset), _)| *offset != layout.proxy.centre && *offset + 12 <= layout.proxy.size.max(0x200))
                .filter(|((_, offset), _)| match (centre, read_vector(game, proxy + offset)) {
                    (Some(c), Some(v)) => (0..3).all(|i| (c[i] - v[i]).abs() < SAME_CENTRE),
                    _ => false,
                })
                .map(|(&key, &count)| (key, count))
                .collect();
            found.collision = copies.iter().max_by_key(|(_, &count)| count).map(|(&key, _)| key);
            let seen: Vec<String> = copies.iter().map(|((bit, o), n)| format!("bit {:#X} -> +{:#X} {}x", bit, o, n)).collect();
            found.notes.push(format!("collision: stores of her centre after bit tests: {}", list(&seen)));
        }
        _ => found.notes.push("collision: her proxy, or where its flags are, isn't known".to_string()),
    }

    // The camera switch: the most compared candidate
    if let Some((path, _, sphere)) = &camera {
        let counts: Vec<(usize, usize)> =
            switches.iter().map(|&o| (o, scan.byte_compares.get(&o).copied().unwrap_or(0))).filter(|(_, n)| *n > 0).collect();
        let seen: Vec<String> = counts.iter().map(|(o, n)| format!("+{:#X} {}x", o, n)).collect();
        found.notes.push(format!("camera: sphere at +{:#X}; set bytes before it the code compares: {}", sphere, list(&seen)));
        found.camera_collision =
            counts.iter().max_by_key(|(o, n)| (*n, *o)).map(|(o, _)| [path.clone(), vec![*o]].concat());
    }

    // The body list's previous link
    let body = proxy.and_then(|p| layout.proxy.body.and_then(|offset| read_pointer(game.handle, p + offset))).filter(|&b| b != 0);
    match body {
        Some(body) => {
            found.previous = find_previous(game, body, layout.body.next);
            found.notes.push(format!("body list: previous link {}", found.previous.map_or("not found".to_string(), |p| format!("+{:#X}", p))));
        }
        None => found.notes.push("body list: her proxy has no body".to_string()),
    }
    found
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

fn read_vector(game: &Game, address: usize) -> Option<[f32; 3]> {
    read_floats(game.handle, address).filter(|v: &[f32; 3]| v.iter().all(|f| f.is_finite()))
}

// The pointers in `span` bytes at `address`, by offset
fn pointers(game: &Game, address: usize, span: usize) -> Vec<(usize, usize)> {
    let size = pointer_size();
    let mut bytes = vec![0u8; span];
    if game.handle.copy_address(address, &mut bytes).is_err() {
        return vec![];
    }
    (0..span).step_by(size).map(|o| (o, crate::game_memory::pointer_from(&bytes[o..]))).filter(|(_, p)| *p != 0).collect()
}

// The path from GameRender to the controller (as offsets, each followed), the controller, and where its sphere is
fn find_controller(game: &Game, classes: &mut Classes) -> Option<(Vec<usize>, usize, usize)> {
    let render = match game.offsets.get(&AddressType::CameraView)?.as_slice() {
        [render, _] => game.pointer_at(&[*render])?,
        _ => return None,
    };
    let is = |classes: &mut Classes, address: usize, class: &str| classes.name(game.handle, address).as_deref() == Some(class);
    for (to_camera, camera) in pointers(game, render, RENDER_SPAN) {
        if !is(classes, camera, CAMERA_COMPONENT) {
            continue;
        }
        for (to_next, next) in pointers(game, camera, CAMERA_SPAN) {
            let mut paths = vec![(vec![to_camera, to_next], next)];
            paths.extend(pointers(game, next, ENTRY_SPAN).into_iter().map(|(o, p)| (vec![to_camera, to_next, o], p)));
            for (path, controller) in paths {
                if is(classes, controller, CONTROLLER) {
                    let sphere = find_sphere(game, classes, controller)?;
                    return Some((path, controller, sphere));
                }
            }
        }
    }
    None
}

// Where the controller's collision sphere is: inline, or a pointer to it
fn find_sphere(game: &Game, classes: &mut Classes, controller: usize) -> Option<usize> {
    let size = pointer_size();
    (size..CONTROLLER_SPAN).step_by(size).find(|&o| {
        classes.name(game.handle, controller + o).as_deref() == Some(SPHERE)
            || read_pointer(game.handle, controller + o)
                .map_or(false, |p| p != 0 && classes.name(game.handle, p).as_deref() == Some(SPHERE))
    })
}

// The offset near `next` at which every body's next points back to it
fn find_previous(game: &Game, body: usize, next: usize) -> Option<usize> {
    let size = pointer_size();
    let candidates = (next.saturating_sub(PREVIOUS_NEAR)..=next + PREVIOUS_NEAR).step_by(size).filter(|&o| o != next);
    candidates
        .filter_map(|previous| {
            let mut current = body;
            let mut checked = 0;
            for _ in 0..PREVIOUS_CHECKS {
                let after = read_pointer(game.handle, current + next).filter(|&p| p != 0)?;
                if read_pointer(game.handle, after + previous) != Some(current) {
                    return None;
                }
                checked += 1;
                current = after;
            }
            (checked > 0).then(|| previous)
        })
        .next()
}

#[derive(Default)]
struct Scan {
    // (bit, offset) of each store through the same register after a byte test of a bit at the flags' offset
    stores_after_tests: BTreeMap<(usize, usize), usize>,
    // Byte compares (cmp/test) at each of the candidate displacements
    byte_compares: BTreeMap<usize, usize>,
}

// One pass over the image's code
fn scan_code(image: &PeImage, flags: Option<usize>, switches: &[usize]) -> Scan {
    let wanted: BTreeSet<usize> = switches.iter().copied().collect();
    let mut scan = Scan::default();
    let displacement = |i: &Instruction| -> i64 {
        if image.is_64 {
            i.memory_displacement64() as i64
        } else {
            i.memory_displacement32() as i32 as i64
        }
    };
    for section in image.sections().iter().filter(|s| s.executable) {
        let end = section.end.min(image.data.len());
        if section.start >= end {
            continue;
        }
        let mut decoder =
            Decoder::with_ip(image.bitness(), &image.data[section.start..end], section.start as u64, DecoderOptions::NONE);
        // Tests waiting for their store: (base register, bit, instructions left)
        let mut waiting: Vec<(Register, usize, usize)> = vec![];
        let mut instruction = Instruction::default();
        while decoder.can_decode() {
            decoder.decode_out(&mut instruction);
            let memory = instruction.op0_kind() == OpKind::Memory;
            let base = instruction.memory_base();
            let based = memory && !matches!(base, Register::None | Register::RIP | Register::EIP);
            if based {
                let d = displacement(&instruction);
                let is_byte = instruction.memory_size() == MemorySize::UInt8;
                if is_byte && matches!(instruction.mnemonic(), Mnemonic::Cmp | Mnemonic::Test) && d >= 0 && wanted.contains(&(d as usize)) {
                    *scan.byte_compares.entry(d as usize).or_default() += 1;
                }
                let is_store = matches!(
                    instruction.mnemonic(),
                    Mnemonic::Mov | Mnemonic::Movaps | Mnemonic::Movups | Mnemonic::Movdqa | Mnemonic::Movdqu | Mnemonic::Movq
                ) && instruction.op1_kind() == OpKind::Register;
                if is_store && d > 0 {
                    for &(register, bit, _) in &waiting {
                        if register == base {
                            *scan.stores_after_tests.entry((bit, d as usize)).or_default() += 1;
                        }
                    }
                }
                let single_bit = instruction.op1_kind() == OpKind::Immediate8 && instruction.immediate8().count_ones() == 1;
                if is_byte && instruction.mnemonic() == Mnemonic::Test && single_bit && flags == Some(d as usize) {
                    waiting.push((base, instruction.immediate8() as usize, STORE_WINDOW));
                }
            }
            waiting.retain_mut(|(_, _, left)| {
                *left -= 1;
                *left > 0
            });
            if matches!(instruction.mnemonic(), Mnemonic::Ret) {
                waiting.clear();
            }
        }
    }
    scan
}

/// `debug flight-fields [--save]` (and `port`)
pub fn find_flight_fields(version: Option<String>, save: bool) -> Result<()> {
    let connection = crate::chains::connect(version)?;
    let mut details = connection.details.clone();
    println!("Reading the game's code...");
    let image = PeImage::dump(connection.pid).context("the game's image couldn't be read")?;
    let game = connection.game();
    let found = find(&game, &image);
    for note in &found.notes {
        println!("  {}", note);
    }
    let before = details.layout.clone();
    let after = found.apply(before.clone());
    let changes = describe_changes(&before, &after);
    if changes.is_empty() {
        println!("The layout already has all of that");
    }
    for change in &changes {
        println!("  {}", change);
    }
    if save && !changes.is_empty() {
        details.layout = after;
        crate::discover::save_entry(crate::offsets_file::VersionEntry::from_details(&details))?;
    }
    Ok(())
}

fn describe_changes(before: &Layout, after: &Layout) -> Vec<String> {
    let hex = |o: Option<usize>| o.map_or("none".to_string(), |o| format!("{:#X}", o));
    let path = |p: &[usize]| if p.is_empty() { "none".to_string() } else { format!("{:X?}", p) };
    let mut changes = vec![];
    let mut field = |name: &str, old: String, new: String| {
        if old != new {
            changes.push(format!("{:<26} {} -> {}", name, old, new));
        }
    };
    field("proxy.movement_collides", hex(before.proxy.movement_collides), hex(after.proxy.movement_collides));
    field("proxy.resolved_centre", hex(before.proxy.resolved_centre), hex(after.proxy.resolved_centre));
    field("camera.collision", path(&before.camera.collision), path(&after.camera.collision));
    field("body.previous", hex(before.body.previous), hex(after.body.previous));
    changes
}
