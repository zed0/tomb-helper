use crate::components::{self, Components};
use crate::layout;
use crate::position::Position;
use crate::ui_ipc_data::TriggerCategory;
use nalgebra::Vector3;
use process_memory::{CopyAddress, ProcessHandle};
use std::convert::TryInto;

// Instances (game objects) are sorted by what they can do through their components, see `components`

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

// Places NPCs and animals play animations at ("animhost_npc_human_eatcandybar", "animhost_npc_deer_bedding", ...)
// have an InteractableComponent, but it's for them rather than Lara
const NOT_INTERACTABLE: [&str; 1] = ["animhost"];

// Whether an instance with an interaction component is one Lara can use, going by its name
pub fn for_lara(name: &str) -> bool {
    let name = name.to_lowercase();
    !NOT_INTERACTABLE.iter().any(|w| name.contains(w))
}

pub fn categorise(name: &str, full_name: &str) -> TriggerCategory {
    let names = format!("{} {}", name, full_name).to_lowercase();
    CATEGORY_WORDS
        .iter()
        .find(|(_, words)| words.iter().any(|w| names.contains(w)))
        .map_or(TriggerCategory::Unknown, |(category, _)| *category)
}

// Where things are in a trigger's shape is in `layout`
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

pub fn read_kinds(handle: ProcessHandle, components: &mut Components, instance_address: usize) -> Kinds {
    let mut has = |class: &str| components.get(handle, instance_address, class).is_some();
    Kinds {
        interactable: has(components::INTERACTABLE) || has(components::LOOT) || has(components::COLLECTIBLE),
        trigger: has(components::TRIGGER),
        npc: has(components::NPC) || (has(components::HEALTH) && has(components::CORPSE)),
    }
}

// The trigger's shape, in its instance's space
pub fn read_shape(handle: ProcessHandle, components: &mut Components, instance_address: usize) -> Option<Shape> {
    let layout = &layout::current().trigger_shape;
    let component = components.get(handle, instance_address, components::SHAPE)?;
    let record = read_usize_at(handle, component + layout.shape)?;
    let mut bytes = vec![0u8; layout.record_size];
    handle.copy_address(record, &mut bytes).ok()?;
    let size = |i: usize| f32_at(&bytes, layout.size + i * 4);
    let shape_type = u32_at(&bytes, layout.shape_type) as usize;
    let shape = if shape_type == layout.box_type {
        Shape::Box(Vector3::new(size(0), size(1), size(2)))
    } else if shape_type == layout.plane_type {
        Shape::Plane(size(0), size(1))
    } else if shape_type == layout.sphere_type {
        Shape::Sphere(size(0))
    } else if shape_type == layout.poly_type {
        let count = u32_at(&bytes, layout.poly_count) as usize;
        if count == 0 || count > MAX_POLY_POINTS {
            return None;
        }
        let mut points = vec![0u8; count * layout.poly_point_size];
        handle.copy_address(usize_at(&bytes, layout.poly_points), &mut points).ok()?;
        Shape::Poly(points.chunks_exact(layout.poly_point_size).map(|p| (vector_at(p, 0), vector_at(p, 16))).collect())
    } else {
        return None;
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
    fn npc_animation_spots_are_not_for_lara() {
        assert!(!for_lara("scenario_vh3_blacksmith.animhost_npc_human_eatcandybar-neberle4"));
        assert!(!for_lara("vh_valley_hub.animhost_npc_deer_bedding-zing44"));
        assert!(for_lara("campsite_base_trx.campsite_survivordifficulty_interact-benriz2"));
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
