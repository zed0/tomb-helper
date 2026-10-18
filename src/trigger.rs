use crate::object_list::INSTANCE_COMPONENTS;
use crate::position::Position;
use crate::ui_ipc_data::TriggerCategory;
use nalgebra::Vector3;
use process_memory::{CopyAddress, ProcessHandle};
use std::collections::HashMap;
use std::convert::TryInto;

// Instances (game objects) are sorted by what they can do through the components in their component table. Each
// component type has a fixed slot, but many instances' tables are shorter, so whatever's in a slot is only taken to
// be that component if its RTTI class name says so. Found in ROTTR (Steam).
const SLOT_TRIGGER: (usize, &str) = (12, ".?AVTriggerComponent@@");
// The trigger's shape
const SLOT_SHAPE: (usize, &str) = (14, ".?AVShapeComponent@cdc@@");
// Campfires, search markers, tethers, throwables, corpses to loot, ...
const SLOT_INTERACTABLE: (usize, &str) = (18, ".?AVInteractableComponent@@");
// Pickups, documents, relics, caches
const SLOT_LOOT: (usize, &str) = (64, ".?AVLootComponent@@");
// Documents, relics, cave markers
const SLOT_COLLECTIBLE: (usize, &str) = (111, ".?AVCollectibleComponent@@");
// Characters and animals with AI (hares, squirrels, chickens, ...)
const SLOT_NPC: (usize, &str) = (31, ".?AVNpcComponent@@");
// Birds have no NpcComponent, but like the rest can be hurt and leave a corpse (Lara has no corpse)
const SLOT_HEALTH: (usize, &str) = (30, ".?AVHealthComponent@cdc@@");
const SLOT_CORPSE: (usize, &str) = (76, ".?AVCorpseComponent@@");

// What triggers are for, by words in their name or the name of what they're part of, checked in this order. Most
// of the game's own names are like "triggerplane_DelayDeath-imill8" or "bendy_bv_young_pine.triggersphere".
const CATEGORY_WORDS: [(TriggerCategory, &[&str]); 8] = [
    (TriggerCategory::Objects, &[
        "bendy", "concealment", "foliage", "bush", "tree", "sapling", "grass", "plant", "collectible", "throwable",
        "campsite", "survivalcache", "looter", "breakable", "balcony", "corpse", "lantern",
    ]),
    (TriggerCategory::Checkpoint, &["checkpoint", "ckpt"]),
    (TriggerCategory::Death, &["death", "kill", "drown"]),
    (TriggerCategory::Streaming, &["streaming", "airlock"]),
    (TriggerCategory::Combat, &[
        "combat", "spawn", "npc", "enemy", "ai_", "wolf", "bear", "deer", "animal", "heli", "stealth", "alert",
    ]),
    (TriggerCategory::Camera, &["camera", "cam_", "customcam", "lookat", "look_at", "headtrack", "interior", "indoor"]),
    (TriggerCategory::Effects, &[
        "sound", "music", "audio", "glow", "dust", "wind", "contrast", "fx", "storm", "weather", "light", "vo_", "bark",
        "tp_",
    ]),
    (TriggerCategory::Hints, &[
        "tutorial", "notification", "hint", "prompt", "radio", "objective", "missing_gear", "discovery", "journal",
    ]),
];

// Things Lara can gather crafting resources from (herb and berry plants, mushrooms, magnesite veins, feathers, ...)
// have no interaction components of their own, but are named "craftable_resource_..." (or e.g.
// "st_martyrdom_connector.craftable_resource_spawner_magnesitevein" when the name includes its parent). Parts of
// them, like "craftable_resource_magnesitevein.live_en_p2p_target", aren't.
const RESOURCE_PREFIX: &str = "craftable_resource";

pub fn is_resource(name: &str) -> bool {
    let own_name = name.rsplit('.').next().unwrap_or(name);
    own_name.to_lowercase().starts_with(RESOURCE_PREFIX)
}

pub fn categorise(name: &str, full_name: &str) -> TriggerCategory {
    let names = format!("{} {}", name, full_name).to_lowercase();
    CATEGORY_WORDS
        .iter()
        .find(|(_, words)| words.iter().any(|w| names.contains(w)))
        .map_or(TriggerCategory::Unknown, |(category, _)| *category)
}

// MSVC RTTI: an object's vtable's entry before the first is a CompleteObjectLocator, which has the RVA of a type
// descriptor with the class name
const LOCATOR_TYPE_DESCRIPTOR: usize = 0xC; // u32 RVA
const TYPE_DESCRIPTOR_NAME: usize = 0x10;
const MAX_CLASS_NAME: usize = 64;

// The shape component points to a record of its shape, in the instance's space
const SHAPE_COMPONENT_SHAPE: usize = 0x18;
const SHAPE_TYPE: usize = 0x0; // u32
const SHAPE_SIZE: usize = 0x8; // Floats, as many as the type needs
const SHAPE_RECORD_SIZE: usize = 0x38;
const TYPE_BOX: u32 = 1; // Half extents x, y, z (unverified whether they're half)
const TYPE_PLANE: u32 = 3; // A rectangle in the x/y plane, half width and height (unverified)
const TYPE_SPHERE: u32 = 4; // Radius
const TYPE_POLY: u32 = 5; // A polygon swept along a vector for each corner ("triggerpolyvolume")
const POLY_COUNT: usize = 0x8; // u32
const POLY_POINTS: usize = 0x10; // -> 0x20 byte entries: corner (x, y, z, 1), then how far it's swept
const POLY_POINT_SIZE: usize = 0x20;
const MAX_POLY_POINTS: usize = 64;
// Lines per circle when drawing spheres
const SPHERE_SEGMENTS: usize = 24;

#[derive(Debug, Clone, Copy, Default)]
pub struct Kinds {
    pub interactable: bool,
    pub trigger: bool,
    pub npc: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Box(Vector3<f32>),
    Plane(f32, f32),
    Sphere(f32),
    Poly(Vec<(Vector3<f32>, Vector3<f32>)>),
}

// Class names by vtable address
pub struct Classes {
    base_address: usize,
    names: HashMap<usize, Option<String>>,
}

impl Classes {
    pub fn new(base_address: usize) -> Classes {
        Classes { base_address, names: HashMap::new() }
    }

    fn name(&mut self, handle: ProcessHandle, object: usize) -> Option<String> {
        let vtable = read_usize_at(handle, object)?;
        let base_address = self.base_address;
        self.names
            .entry(vtable)
            .or_insert_with(|| {
                let locator = read_usize_at(handle, vtable.checked_sub(8)?)?;
                let mut rva = [0u8; 4];
                handle.copy_address(locator + LOCATOR_TYPE_DESCRIPTOR, &mut rva).ok()?;
                let mut name = [0u8; MAX_CLASS_NAME];
                let descriptor = base_address + u32::from_le_bytes(rva) as usize;
                handle.copy_address(descriptor + TYPE_DESCRIPTOR_NAME, &mut name).ok()?;
                let end = name.iter().position(|&c| c == 0)?;
                Some(String::from_utf8_lossy(&name[..end]).into_owned())
            })
            .clone()
    }

    // The instance's component in the slot, if it's that one
    fn component(&mut self, handle: ProcessHandle, table: usize, (slot, class): (usize, &str)) -> Option<usize> {
        let component = read_usize_at(handle, table + slot * 8)?;
        match self.name(handle, component) {
            Some(name) if name == class => Some(component),
            _ => None,
        }
    }
}

pub fn read_kinds(handle: ProcessHandle, classes: &mut Classes, instance_address: usize) -> Kinds {
    let table = match read_usize_at(handle, instance_address + INSTANCE_COMPONENTS) {
        Some(t) => t,
        None => return Kinds::default(),
    };
    Kinds {
        interactable: [SLOT_INTERACTABLE, SLOT_LOOT, SLOT_COLLECTIBLE]
            .iter()
            .any(|&slot| classes.component(handle, table, slot).is_some()),
        trigger: classes.component(handle, table, SLOT_TRIGGER).is_some(),
        npc: classes.component(handle, table, SLOT_NPC).is_some()
            || (classes.component(handle, table, SLOT_HEALTH).is_some()
                && classes.component(handle, table, SLOT_CORPSE).is_some()),
    }
}

// The trigger's shape, in its instance's space
pub fn read_shape(handle: ProcessHandle, classes: &mut Classes, instance_address: usize) -> Option<Shape> {
    let table = read_usize_at(handle, instance_address + INSTANCE_COMPONENTS)?;
    let component = classes.component(handle, table, SLOT_SHAPE)?;
    let record = read_usize_at(handle, component + SHAPE_COMPONENT_SHAPE)?;
    let mut bytes = [0u8; SHAPE_RECORD_SIZE];
    handle.copy_address(record, &mut bytes).ok()?;
    let size = |i: usize| f32_at(&bytes, SHAPE_SIZE + i * 4);
    let shape = match u32_at(&bytes, SHAPE_TYPE) {
        TYPE_BOX => Shape::Box(Vector3::new(size(0), size(1), size(2))),
        TYPE_PLANE => Shape::Plane(size(0), size(1)),
        TYPE_SPHERE => Shape::Sphere(size(0)),
        TYPE_POLY => {
            let count = u32_at(&bytes, POLY_COUNT) as usize;
            if count == 0 || count > MAX_POLY_POINTS {
                return None;
            }
            let mut points = vec![0u8; count * POLY_POINT_SIZE];
            handle.copy_address(usize_at(&bytes, POLY_POINTS), &mut points).ok()?;
            Shape::Poly(points.chunks_exact(POLY_POINT_SIZE).map(|p| (vector_at(p, 0), vector_at(p, 16))).collect())
        }
        _ => return None,
    };
    let sane = |v: f32| v.is_finite() && v.abs() < 1e6;
    let ok = match &shape {
        Shape::Box(half) => half.iter().all(|&v| sane(v)),
        Shape::Plane(x, y) => sane(*x) && sane(*y),
        Shape::Sphere(radius) => sane(*radius),
        Shape::Poly(points) => points.iter().all(|(p, e)| p.iter().chain(e.iter()).all(|&v| sane(v))),
    };
    if ok {
        Some(shape)
    } else {
        None
    }
}

// The shape's outline as world space lines, for a row-major object-to-world matrix of row vectors
pub fn outline(shape: &Shape, matrix: &[f32; 16]) -> Vec<(Position, Position)> {
    let row = |r: usize| Vector3::new(matrix[r * 4], matrix[r * 4 + 1], matrix[r * 4 + 2]);
    let world = |p: Vector3<f32>| {
        let w = row(0) * p.x + row(1) * p.y + row(2) * p.z + row(3);
        Position { x: w.x, y: w.y, z: w.z }
    };
    let mut lines = vec![];
    let mut line = |a: Vector3<f32>, b: Vector3<f32>| lines.push((world(a), world(b)));
    match shape {
        Shape::Box(half) => {
            let corner = |i: usize| {
                Vector3::new(
                    if i & 1 != 0 { half.x } else { -half.x },
                    if i & 2 != 0 { half.y } else { -half.y },
                    if i & 4 != 0 { half.z } else { -half.z },
                )
            };
            for a in 0..8 {
                for axis in [1, 2, 4] {
                    if a & axis == 0 {
                        line(corner(a), corner(a | axis));
                    }
                }
            }
        }
        Shape::Plane(x, y) => {
            let corners = [
                Vector3::new(-x, -y, 0.0),
                Vector3::new(*x, -y, 0.0),
                Vector3::new(*x, *y, 0.0),
                Vector3::new(-x, *y, 0.0),
            ];
            for i in 0..4 {
                line(corners[i], corners[(i + 1) % 4]);
            }
            // So it reads as a surface
            line(corners[0], corners[2]);
        }
        Shape::Sphere(radius) => {
            let angle = |i: usize| i as f32 * std::f32::consts::TAU / SPHERE_SEGMENTS as f32;
            let axes = [(Vector3::x(), Vector3::y()), (Vector3::y(), Vector3::z()), (Vector3::z(), Vector3::x())];
            for (u, v) in axes {
                let point = |i: usize| (u * angle(i).cos() + v * angle(i).sin()) * *radius;
                for i in 0..SPHERE_SEGMENTS {
                    line(point(i), point(i + 1));
                }
            }
        }
        Shape::Poly(points) => {
            for (i, (point, sweep)) in points.iter().enumerate() {
                let (next, next_sweep) = &points[(i + 1) % points.len()];
                line(*point, *next);
                line(point + sweep, next + next_sweep);
                line(*point, point + sweep);
            }
        }
    }
    lines
}

fn read_usize_at(handle: ProcessHandle, address: usize) -> Option<usize> {
    let mut bytes = [0u8; 8];
    handle.copy_address(address, &mut bytes).ok()?;
    match usize_at(&bytes, 0) {
        0 => None,
        pointer => Some(pointer),
    }
}

fn usize_at(bytes: &[u8], offset: usize) -> usize {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn f32_at(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn vector_at(bytes: &[u8], offset: usize) -> Vector3<f32> {
    Vector3::new(f32_at(bytes, offset), f32_at(bytes, offset + 4), f32_at(bytes, offset + 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [f32; 16] = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];

    #[test]
    fn triggers_are_categorised_by_their_names() {
        let category = |name, full_name| categorise(name, full_name);
        assert_eq!(category("triggerplane_DelayDeath-imill8", ""), TriggerCategory::Death);
        assert_eq!(category("triggersphere", "bendy_bv_young_pine.triggersphere-pigagne3"), TriggerCategory::Objects);
        assert_eq!(category("triggersphere_lookat-jherbert1", ""), TriggerCategory::Camera);
        assert_eq!(category("streaming_cm_office_start-ryeo1", ""), TriggerCategory::Streaming);
        assert_eq!(category("triggerplane", "ge_gulag_escape_master.triggerplane-josh3"), TriggerCategory::Unknown);
    }

    #[test]
    fn resources_are_found_by_name_but_not_their_parts() {
        assert!(is_resource("craftable_resource_spawner_herbs_plant_a-athayer19"));
        assert!(is_resource("st_martyrdom_connector.craftable_resource_spawner_magnesitevein-x1"));
        assert!(!is_resource("craftable_resource_magnesitevein.live_en_p2p_target-x2"));
        assert!(!is_resource("marker_object_lookat_resources-x3"));
    }

    #[test]
    fn outlines_have_the_right_number_of_lines() {
        assert_eq!(outline(&Shape::Box(Vector3::new(1., 2., 3.)), &IDENTITY).len(), 12);
        assert_eq!(outline(&Shape::Plane(1., 2.), &IDENTITY).len(), 5);
        assert_eq!(outline(&Shape::Sphere(5.), &IDENTITY).len(), 3 * SPHERE_SEGMENTS);
        let up = Vector3::new(0., 0., 10.);
        let poly = Shape::Poly(vec![(Vector3::zeros(), up), (Vector3::x(), up), (Vector3::y(), up)]);
        assert_eq!(outline(&poly, &IDENTITY).len(), 9);
    }

    #[test]
    fn outlines_are_placed_by_the_matrix() {
        // Moved by (100, 0, 0)
        let mut matrix = IDENTITY;
        matrix[12] = 100.;
        let lines = outline(&Shape::Sphere(5.), &matrix);
        assert!(lines.iter().all(|(a, _)| (((a.x - 100.).powi(2) + a.y.powi(2) + a.z.powi(2)).sqrt() - 5.).abs() < 1e-3));
    }
}
