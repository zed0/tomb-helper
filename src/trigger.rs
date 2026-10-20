use crate::game_memory::{Handle, f32_at, pointer_at, read_pointer, read_u32, u32_at, vector_at};
use crate::components::{self, Components};
use crate::layout;
use crate::position::Position;
use crate::ui_ipc_data::TriggerCategory;
use nalgebra::{Matrix3, Vector3};
use process_memory::CopyAddress;

// Instances (game objects) are sorted by what they can do through their components, see `components`

// The words that say what things are are in `words`


// Whether an instance with an interaction component is one Lara can use, going by its name
pub fn for_lara(name: &str) -> bool {
    let name = name.to_lowercase();
    !crate::words::current().not_interactable.iter().any(|w| name.contains(w.as_str()))
}

pub fn categorise(name: &str, full_name: &str) -> TriggerCategory {
    let names = format!("{} {}", name, full_name).to_lowercase();
    let words = crate::words::current();
    let found = words.categories().find(|(_, words)| words.iter().any(|w| names.contains(w.as_str())));
    found.map_or(TriggerCategory::Unknown, |(category, _)| category)
}

// Where things are in a trigger's shape is in `layout`
const MAX_POLY_POINTS: usize = 64;
// Lines per circle when drawing spheres
const SPHERE_SEGMENTS: usize = 24;
// The filled sphere's slices round and from pole to pole
const SPHERE_FACE_SEGMENTS: usize = 12;
const SPHERE_FACE_RINGS: usize = 6;

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

// Components listed at most
const MAX_COMPONENT_DATA: usize = 64;

pub fn read_kinds(handle: Handle, components: &mut Components, instance_address: usize) -> Kinds {
    let layout = layout::current();
    if let (None, Some(offset)) = (layout.instance.components, layout.instance.component_data) {
        let names = read_component_names(handle, instance_address + offset);
        let words = crate::words::current();
        let any = |wanted: &[String]| names.iter().any(|n| wanted.contains(n));
        return Kinds {
            interactable: any(&words.interactable_components) && !any(&words.inventory_components),
            trigger: trigger_object(handle, components, instance_address).is_some(),
            npc: any(&words.npc_components),
        };
    }
    let mut has = |class: &str| components.get(handle, instance_address, class).is_some();
    Kinds {
        interactable: has(components::INTERACTABLE) || has(components::LOOT) || has(components::COLLECTIBLE),
        trigger: has(components::TRIGGER),
        npc: has(components::NPC) || (has(components::HEALTH) && has(components::CORPSE)),
    }
}

// The names of the components in an instance's component data, in lower case
fn read_component_names(handle: Handle, pointer_address: usize) -> Vec<String> {
    let layout = &layout::current().component_data;
    let data = match read_pointer(handle, pointer_address) {
        Some(data) => data,
        None => return vec![],
    };
    let count = match read_u32(handle, data + layout.count) {
        Some(count) => (count as usize).min(MAX_COMPONENT_DATA),
        None => return vec![],
    };
    let mut entries = vec![0u8; count * layout.entry_size];
    if handle.copy_address(data + layout.entries, &mut entries).is_err() {
        return vec![];
    }
    entries
        .chunks_exact(layout.entry_size)
        .filter_map(|entry| {
            let mut name = [0u8; 48];
            handle.copy_address(pointer_at(entry, layout.name), &mut name).ok()?;
            let end = name.iter().position(|&c| c == 0)?;
            Some(String::from_utf8_lossy(&name[..end]).to_lowercase())
        })
        .collect()
}

// Where triggers are game objects (TR2013), the instance's object and whether it's a volume (else a plane), if it's a
// trigger
fn trigger_object(handle: Handle, components: &mut Components, instance_address: usize) -> Option<(usize, bool)> {
    let offset = layout::current().instance.game_object?;
    let object = read_pointer(handle, instance_address + offset)?;
    match components.classes.name(handle, object)?.as_str() {
        components::TRIGGER_VOLUME => Some((object, true)),
        components::TRIGGER_PLANE => Some((object, false)),
        _ => None,
    }
}

// A trigger's shape from its game object (TR2013)
fn read_object_shape(handle: Handle, object: usize, volume: bool) -> Option<Shape> {
    let layout = &layout::current().trigger_object;
    let mut bytes = vec![0u8; layout.volume_size.max(layout.plane_size) + 0xC];
    handle.copy_address(object, &mut bytes).ok()?;
    if !volume {
        return Some(Shape::Plane(f32_at(&bytes, layout.plane_size), f32_at(&bytes, layout.plane_size + 4)));
    }
    let shape_type = u32_at(&bytes, layout.volume_type) as usize;
    let size = vector_at(&bytes, layout.volume_size);
    if shape_type == layout.volume_box {
        Some(Shape::Box(size))
    } else if shape_type == layout.volume_sphere {
        Some(Shape::Sphere(size.x))
    } else {
        None
    }
}

// The trigger's shape, in its instance's space
pub fn read_shape(handle: Handle, components: &mut Components, instance_address: usize) -> Option<Shape> {
    let shape = match trigger_object(handle, components, instance_address) {
        Some((object, volume)) => read_object_shape(handle, object, volume),
        None => read_component_shape(handle, components, instance_address),
    }?;
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

// A trigger's shape from its ShapeComponent (ROTTR, SOTTR)
fn read_component_shape(handle: Handle, components: &mut Components, instance_address: usize) -> Option<Shape> {
    let layout = &layout::current().trigger_shape;
    let component = components.get(handle, instance_address, components::SHAPE)?;
    let record = read_pointer(handle, component + layout.shape)?;
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
        handle.copy_address(pointer_at(&bytes, layout.poly_points), &mut points).ok()?;
        Shape::Poly(points.chunks_exact(layout.poly_point_size).map(|p| (vector_at(p, 0), vector_at(p, 16))).collect())
    } else {
        return None;
    };
    Some(shape)
}

// The shape's outline as world space lines, for a row-major object-to-world matrix of row vectors
pub fn outline(shape: &Shape, matrix: &[f32; 16]) -> Vec<(Position, Position)> {
    let row = |r: usize| Vector3::new(matrix[r * 4], matrix[r * 4 + 1], matrix[r * 4 + 2]);
    let world = |p: Vector3<f32>| {
        let w = row(0) * p.x + row(1) * p.y + row(2) * p.z + row(3);
        Position::from(w)
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
        Shape::Plane(x, z) => {
            let corners = plane_corners(*x, *z);
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

// A plane is upright in its instance's space: half its width along x, half its height along z (going by the bounds the
// game keeps for it, TriggerComponent +0xC0/+0xD0)
fn plane_corners(x: f32, z: f32) -> [Vector3<f32>; 4] {
    [Vector3::new(-x, 0.0, -z), Vector3::new(x, 0.0, -z), Vector3::new(x, 0.0, z), Vector3::new(-x, 0.0, z)]
}

/// Whether a world space point is inside the shape (never for a plane, which has no inside), for a row-major
/// object-to-world matrix of row vectors
pub fn contains(shape: &Shape, matrix: &[f32; 16], point: &Position) -> bool {
    // Back into the shape's space: solve local * M3 + translation = point
    let m = Matrix3::new(
        matrix[0], matrix[4], matrix[8], matrix[1], matrix[5], matrix[9], matrix[2], matrix[6], matrix[10],
    );
    let offset = Vector3::new(point.x - matrix[12], point.y - matrix[13], point.z - matrix[14]);
    let local = match m.try_inverse() {
        Some(inverse) => inverse * offset,
        None => return false,
    };
    match shape {
        Shape::Box(half) => local.iter().zip(half.iter()).all(|(p, h)| p.abs() <= h.abs()),
        Shape::Plane(_, _) => false,
        Shape::Sphere(radius) => local.norm() <= *radius,
        Shape::Poly(points) => {
            // Between the polygon and where it's swept to (taking the sweep as the same for every corner), and inside
            // the polygon seen along the sweep
            let sweep = match points.first() {
                Some((_, sweep)) if sweep.norm() > 0.0 => *sweep,
                _ => return false,
            };
            let along = |p: &Vector3<f32>| p.dot(&sweep) / sweep.norm_squared();
            let base = points.iter().map(|(p, _)| along(p)).sum::<f32>() / points.len() as f32;
            let t = along(&local) - base;
            if !(0.0..=1.0).contains(&t) {
                return false;
            }
            let u = sweep.cross(&if sweep.x.abs() < 0.9 { Vector3::x() } else { Vector3::y() }).normalize();
            let v = sweep.normalize().cross(&u);
            let flat = |p: &Vector3<f32>| (p.dot(&u), p.dot(&v));
            let (x, y) = flat(&local);
            // Crossings of a ray along +u
            let mut inside = false;
            for (i, (a, _)) in points.iter().enumerate() {
                let ((ax, ay), (bx, by)) = (flat(a), flat(&points[(i + 1) % points.len()].0));
                if (ay > y) != (by > y) && x < ax + (y - ay) * (bx - ax) / (by - ay) {
                    inside = !inside;
                }
            }
            inside
        }
    }
}

// The shape's surface as world space triangles (winding not consistent), for a row-major object-to-world matrix of row
// vectors
pub fn faces(shape: &Shape, matrix: &[f32; 16]) -> Vec<[Position; 3]> {
    let row = |r: usize| Vector3::new(matrix[r * 4], matrix[r * 4 + 1], matrix[r * 4 + 2]);
    let world = |p: Vector3<f32>| {
        let w = row(0) * p.x + row(1) * p.y + row(2) * p.z + row(3);
        Position::from(w)
    };
    let mut faces = vec![];
    let quad = |a: Vector3<f32>, b: Vector3<f32>, c: Vector3<f32>, d: Vector3<f32>| {
        [[world(a), world(b), world(c)], [world(a), world(c), world(d)]]
    };
    match shape {
        Shape::Box(half) => {
            let corner = |i: usize| {
                Vector3::new(
                    if i & 1 != 0 { half.x } else { -half.x },
                    if i & 2 != 0 { half.y } else { -half.y },
                    if i & 4 != 0 { half.z } else { -half.z },
                )
            };
            // Each side is the corners with one axis' bit fixed, in order round it
            for (axis, others) in [(1, [2, 4]), (2, [1, 4]), (4, [1, 2])] {
                for side in [0, axis] {
                    let [u, v] = others;
                    faces.extend(quad(corner(side), corner(side | u), corner(side | u | v), corner(side | v)));
                }
            }
        }
        Shape::Plane(x, z) => {
            let [a, b, c, d] = plane_corners(*x, *z);
            faces.extend(quad(a, b, c, d))
        }
        Shape::Sphere(radius) => {
            let point = |segment: usize, ring: usize| {
                let around = segment as f32 * std::f32::consts::TAU / SPHERE_FACE_SEGMENTS as f32;
                let down = ring as f32 * std::f32::consts::PI / SPHERE_FACE_RINGS as f32;
                Vector3::new(down.sin() * around.cos(), down.sin() * around.sin(), down.cos()) * *radius
            };
            for ring in 0..SPHERE_FACE_RINGS {
                for segment in 0..SPHERE_FACE_SEGMENTS {
                    let (a, b) = (point(segment, ring), point(segment + 1, ring));
                    let (c, d) = (point(segment + 1, ring + 1), point(segment, ring + 1));
                    // The rings at the poles are points, so those quads are triangles
                    if ring == 0 {
                        faces.push([world(a), world(c), world(d)]);
                    } else if ring == SPHERE_FACE_RINGS - 1 {
                        faces.push([world(a), world(b), world(c)]);
                    } else {
                        faces.extend(quad(a, b, c, d));
                    }
                }
            }
        }
        Shape::Poly(points) => {
            for (i, (point, sweep)) in points.iter().enumerate() {
                let (next, next_sweep) = &points[(i + 1) % points.len()];
                faces.extend(quad(*point, *next, next + next_sweep, point + sweep));
            }
            let bottom: Vec<Vector3<f32>> = points.iter().map(|(p, _)| *p).collect();
            let top: Vec<Vector3<f32>> = points.iter().map(|(p, s)| p + s).collect();
            for cap in [bottom, top] {
                for [a, b, c] in triangulate(&cap) {
                    faces.push([world(cap[a]), world(cap[b]), world(cap[c])]);
                }
            }
        }
    }
    faces
}

// A polygon (roughly flat, maybe concave) as triangles of its corners' indices, by clipping off ears
fn triangulate(polygon: &[Vector3<f32>]) -> Vec<[usize; 3]> {
    if polygon.len() < 3 {
        return vec![];
    }
    // Newell's normal, which also says which way round the corners go
    let mut normal = Vector3::zeros();
    for (i, a) in polygon.iter().enumerate() {
        let b = polygon[(i + 1) % polygon.len()];
        normal += Vector3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
    }
    let convex = |a: Vector3<f32>, b: Vector3<f32>, c: Vector3<f32>| (b - a).cross(&(c - b)).dot(&normal) > 0.0;
    // Including on its edges, so an ear can't have another corner on its far side
    let left_of = |a: Vector3<f32>, b: Vector3<f32>, p: Vector3<f32>| (b - a).cross(&(p - b)).dot(&normal) >= 0.0;
    let inside = |p: Vector3<f32>, a: Vector3<f32>, b: Vector3<f32>, c: Vector3<f32>| {
        left_of(a, b, p) && left_of(b, c, p) && left_of(c, a, p)
    };
    let mut left: Vec<usize> = (0..polygon.len()).collect();
    let mut triangles = vec![];
    while left.len() > 3 {
        let n = left.len();
        let ear = (0..n).find(|&i| {
            let (a, b, c) = (left[(i + n - 1) % n], left[i], left[(i + 1) % n]);
            convex(polygon[a], polygon[b], polygon[c])
                && !left.iter().any(|&p| ![a, b, c].contains(&p) && inside(polygon[p], polygon[a], polygon[b], polygon[c]))
        });
        // None if it's twisted or degenerate; just fan the rest
        let i = match ear {
            Some(i) => i,
            None => break,
        };
        triangles.push([left[(i + n - 1) % n], left[i], left[(i + 1) % n]]);
        left.remove(i);
    }
    for i in 1..left.len() - 1 {
        triangles.push([left[0], left[i], left[i + 1]]);
    }
    triangles
}






#[cfg(test)]
mod tests {
    use super::*;

    fn area(faces: &[[Position; 3]]) -> f32 {
        faces.iter().map(|[a, b, c]| (b.vector() - a.vector()).cross(&(c.vector() - a.vector())).norm() / 2.0).sum()
    }

    #[test]
    fn points_inside_shapes() {
        let p = |x, y, z| Position { x, y, z };
        // Scaled by 2 and moved to (100, 0, 0)
        let matrix = [2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 100.0, 0.0, 0.0, 1.0];
        let cube = Shape::Box(Vector3::new(1.0, 1.0, 1.0));
        assert!(contains(&cube, &matrix, &p(101.5, -1.5, 1.9)));
        assert!(!contains(&cube, &matrix, &p(102.5, 0.0, 0.0)));
        assert!(contains(&Shape::Sphere(3.0), &matrix, &p(100.0, 5.0, 0.0)));
        // (2, 2.5, 0) in its space
        assert!(!contains(&Shape::Sphere(3.0), &matrix, &p(104.0, 5.0, 0.0)));
        assert!(!contains(&Shape::Plane(10.0, 10.0), &matrix, &p(100.0, 0.0, 0.0)));
        // An L shaped prism 10 high
        let l = [(0.0, 0.0), (2.0, 0.0), (2.0, 1.0), (1.0, 1.0), (1.0, 2.0), (0.0, 2.0)];
        let prism = Shape::Poly(l.iter().map(|&(x, y)| (Vector3::new(x, y, 0.0), Vector3::new(0.0, 0.0, 10.0))).collect());
        assert!(contains(&prism, &IDENTITY, &p(0.5, 1.5, 5.0)));
        assert!(contains(&prism, &IDENTITY, &p(1.5, 0.5, 9.0)));
        assert!(!contains(&prism, &IDENTITY, &p(1.5, 1.5, 5.0)));
        assert!(!contains(&prism, &IDENTITY, &p(0.5, 0.5, 11.0)));
    }

    #[test]
    fn planes_are_upright() {
        let corners: Vec<Position> = faces(&Shape::Plane(1.0, 2.0), &IDENTITY).into_iter().flatten().collect();
        assert!(corners.iter().all(|c| c.y == 0.0 && c.x.abs() == 1.0 && c.z.abs() == 2.0));
    }

    #[test]
    fn box_faces_cover_its_sides() {
        let faces = faces(&Shape::Box(Vector3::new(1.0, 2.0, 3.0)), &IDENTITY);
        assert_eq!(faces.len(), 12);
        // 2 * (2*4 + 2*6 + 4*6)
        assert!((area(&faces) - 88.0).abs() < 1e-3, "{}", area(&faces));
    }

    #[test]
    fn sphere_faces_are_nearly_its_area() {
        let faces = faces(&Shape::Sphere(10.0), &IDENTITY);
        let sphere = 4.0 * std::f32::consts::PI * 100.0;
        assert!(area(&faces) < sphere && area(&faces) > sphere * 0.9, "{}", area(&faces));
    }

    #[test]
    fn concave_caps_are_triangulated_inside_the_polygon() {
        // An L shape, area 3
        let l = [(0.0, 0.0), (2.0, 0.0), (2.0, 1.0), (1.0, 1.0), (1.0, 2.0), (0.0, 2.0)];
        for points in [l.to_vec(), l.iter().rev().copied().collect()] {
            let polygon: Vec<Vector3<f32>> = points.iter().map(|&(x, y)| Vector3::new(x, y, 5.0)).collect();
            let triangles = triangulate(&polygon);
            assert_eq!(triangles.len(), 4);
            let faces: Vec<[Position; 3]> = triangles
                .iter()
                .map(|t| t.map(|i| Position { x: polygon[i].x, y: polygon[i].y, z: polygon[i].z }))
                .collect();
            assert!((area(&faces) - 3.0).abs() < 1e-4, "{}", area(&faces));
        }
    }

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
