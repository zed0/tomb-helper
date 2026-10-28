//! The camera's fields (`layout.camera`, in GameRender, which `CameraView`'s chain starts at) by what they have to be,
//! which holds in every game of the engine (checked in ROTTR, SOTTR 492 and TR2013 Steam, 32 and 64-bit): `view` is a
//! world-to-view matrix, the inverse of one of GameRender's camera-to-world matrices, all of which put the camera near
//! Lara (where the world-to-view one, read as camera-to-world, doesn't); `sin` (and `cos` after it) starts the first of
//! those camera-to-world matrices; and the lens has the vertical fov just before the aspect ratio, with the near and far
//! planes before those. Matrices are row-major for row vectors, so a transform's last column is (0, 0, 0, 1).
//!
//! The layout's own offsets are kept where they pass (ROTTR's lens has two copies of the fov and aspect, and the first
//! is sometimes another render pass's), so fields are only looked for where they don't. `apply` (for `update-offsets`),
//! `check` (`verify`), `debug camera-fields [--layout-of <version>]`.

use anyhow::{Context, Result};
use crate::game::Game;
use crate::game_memory::{f32_at, floats_at, resolve};
use crate::layout::{CameraLayout, Layout};
use crate::process_details::AddressType;
use nalgebra::{Matrix4, Vector3};
use process_memory::CopyAddress;

// How much of GameRender is looked through
const SPAN: usize = 0x1000;
// A camera-to-world matrix puts the camera this close to Lara
const NEAR_LARA: f32 = 2000.0;
// What a vertical fov (radians), an aspect ratio and a near plane can be: (more than, less than)
const FOV: (f32, f32) = (0.3, 2.0);
const ASPECT: (f32, f32) = (1.0, 3.0);
const NEAR_PLANE: (f32, f32) = (0.001, 1000.0);
// The far plane is at least this many times as far as the near one
const FAR_RATIO: f32 = 10.0;
// The floats `CameraLens` reads, which `lens_*` index
const LENS_FLOATS: usize = 8;
// How close a product of two matrices has to be to the identity for them to be inverses, and a row to unit length
const TOLERANCE: f32 = 0.01;
// How close one's translation has to come to undoing the other's, for how far they are from the origin (float
// rounding through a rotation, there)
const TRANSLATION_TOLERANCE: f32 = 0.001;
// Size of a matrix
const MATRIX: usize = 64;

fn within(value: f32, (above, below): (f32, f32)) -> bool {
    value > above && value < below
}

// GameRender, and as much of it as can be read (None if that isn't past `needed`, as in a snapshot, which only has
// what `check` read)
fn game_render(game: &Game, needed: usize) -> Option<(usize, Vec<u8>)> {
    let render = match game.offsets.get(&AddressType::CameraView)?.as_slice() {
        [render, _] => resolve(game.handle, game.base_address, &[*render, 0])?,
        _ => return None,
    };
    // As much as can be read (a read past the end of what's mapped fails), in steps of a matrix
    (1..=SPAN / MATRIX).rev().map(|n| n * MATRIX).filter(|size| *size >= needed).find_map(|size| {
        let mut bytes = vec![0u8; size];
        game.handle.copy_address(render, &mut bytes).ok().map(|_| (render, bytes))
    })
}

// The matrix at `offset` if it's a transform whose 3x3 part is a rotation
fn transform(bytes: &[u8], offset: usize) -> Option<[f32; 16]> {
    if offset + MATRIX > bytes.len() {
        return None;
    }
    let m: [f32; 16] = floats_at(bytes, offset);
    let last_column = [m[3], m[7], m[11], m[15] - 1.0];
    if !m.iter().all(|v| v.is_finite() && v.abs() < 1e7) || last_column.iter().any(|v| v.abs() > TOLERANCE) {
        return None;
    }
    let rows = [0, 4, 8].map(|i| Vector3::new(m[i], m[i + 1], m[i + 2]));
    let unit = rows.iter().all(|r| (r.norm_squared() - 1.0).abs() <= TOLERANCE);
    let square = (0..3).all(|i| (i + 1..3).all(|j| rows[i].dot(&rows[j]).abs() <= TOLERANCE));
    (unit && square).then(|| m)
}

// Where it puts the camera, read as camera-to-world: how far from Lara
fn camera_distance(m: &[f32; 16], lara: &Vector3<f32>) -> f32 {
    (Vector3::new(m[12], m[13], m[14]) - lara).norm()
}

// Whether `a` and `b` (row-major transforms for row vectors) undo each other. Their rotations have to multiply to the
// identity, and `a`'s translation taken through `b` has to cancel `b`'s, to within float precision of how far from
// the origin they are (TR2013's positions are ~30000, where floats are only good to a few thousandths)
fn inverses(a: &[f32; 16], b: &[f32; 16]) -> bool {
    // Read column-major, these are the transposes, which act on column vectors the same way
    let (a, b) = (Matrix4::from_column_slice(a), Matrix4::from_column_slice(b));
    let rotation = a.fixed_view::<3, 3>(0, 0) * b.fixed_view::<3, 3>(0, 0);
    if (rotation - nalgebra::Matrix3::identity()).iter().any(|d| d.abs() > TOLERANCE) {
        return false;
    }
    let (a_translation, b_translation) = (a.fixed_view::<3, 1>(0, 3).into_owned(), b.fixed_view::<3, 1>(0, 3).into_owned());
    let left = b.fixed_view::<3, 3>(0, 0) * a_translation + b_translation;
    left.norm() <= TRANSLATION_TOLERANCE * a_translation.norm().max(b_translation.norm()).max(1.0)
}

/// What GameRender has, to check offsets against
pub struct Camera {
    bytes: Vec<u8>,
    lara: Vector3<f32>,
    // The transforms in it, by offset
    transforms: Vec<(usize, [f32; 16])>,
}

impl Camera {
    /// GameRender as far past the layout's camera fields as can be read, if that's past them all
    pub fn read(game: &Game, camera: &CameraLayout) -> Option<Camera> {
        let needed = (camera.view + MATRIX).max(camera.sin + MATRIX).max(camera.lens + LENS_FLOATS * 4);
        let (_, bytes) = game_render(game, needed)?;
        let lara = game.lara_position().ok()?.vector();
        let transforms = (0..bytes.len().saturating_sub(MATRIX)).step_by(4).filter_map(|o| Some((o, transform(&bytes, o)?))).collect();
        Some(Camera { bytes, lara, transforms })
    }

    fn near_lara(&self, m: &[f32; 16]) -> bool {
        camera_distance(m, &self.lara) < NEAR_LARA
    }

    /// Whether `offset` is the world-to-view matrix: the inverse of a camera-to-world one, and not one itself
    pub fn is_view(&self, offset: usize) -> bool {
        match transform(&self.bytes, offset) {
            Some(view) if !self.near_lara(&view) => {
                self.transforms.iter().any(|(o, m)| *o != offset && self.near_lara(m) && inverses(m, &view))
            }
            _ => false,
        }
    }

    /// Whether `offset` starts a camera-to-world matrix (`sin` is its first row's first float)
    pub fn is_to_world(&self, offset: usize) -> bool {
        transform(&self.bytes, offset).map_or(false, |m| self.near_lara(&m))
    }

    /// Whether the lens fields read a near plane, a far one, a fov and an aspect ratio
    pub fn is_lens(&self, camera: &CameraLayout) -> bool {
        let float = |i: usize| (camera.lens + i * 4 + 4 <= self.bytes.len() && i < LENS_FLOATS).then(|| f32_at(&self.bytes, camera.lens + i * 4));
        match (float(camera.lens_near), float(camera.lens_far), float(camera.lens_fov), float(camera.lens_aspect)) {
            (Some(near), Some(far), Some(fov), Some(aspect)) => {
                within(near, NEAR_PLANE) && far > near * FAR_RATIO && within(fov, FOV) && within(aspect, ASPECT)
            }
            _ => false,
        }
    }

    // The world-to-view matrix, if just one is
    fn find_view(&self) -> Option<usize> {
        let views: Vec<usize> = self.transforms.iter().map(|(o, _)| *o).filter(|o| self.is_view(*o)).collect();
        (views.len() == 1).then(|| views[0])
    }

    // The first camera-to-world matrix
    fn find_to_world(&self) -> Option<usize> {
        self.transforms.iter().map(|(o, _)| *o).find(|o| self.is_to_world(*o))
    }

    // The lens: the first fov and aspect next to each other, with the near and far planes before them, in the
    // `LENS_FLOATS` floats from `lens` (kept where they fit, else starting so the planes come first)
    fn find_lens(&self, camera: &CameraLayout) -> Option<CameraLayout> {
        let float = |o: usize| f32_at(&self.bytes, o);
        let fov = (8..self.bytes.len().saturating_sub(8)).step_by(4).find(|&o| within(float(o), FOV) && within(float(o + 4), ASPECT))?;
        let lens = if camera.lens <= fov && fov + 4 < camera.lens + LENS_FLOATS * 4 { camera.lens } else { fov - 8 };
        let index = |o: usize| (o - lens) / 4;
        // The planes: the last pair before the fov that can be
        let planes = (lens..fov - 4).step_by(4).rev().find(|&o| within(float(o), NEAR_PLANE) && float(o + 4) > float(o) * FAR_RATIO)?;
        Some(CameraLayout {
            lens,
            lens_near: index(planes),
            lens_far: index(planes + 4),
            lens_fov: index(fov),
            lens_aspect: index(fov + 4),
            ..camera.clone()
        })
    }
}

// A note on a field that moved
fn moved(name: &str, old: usize, new: usize, why: &str) -> String {
    format!("{:<34} {:#X} -> {:#X} ({})", name, old, new, why)
}

/// The camera's fields, kept where they pass and found where they don't, with notes on what changed. None if
/// GameRender or Lara's position can't be read.
pub fn derive(game: &Game, layout: &Layout) -> Option<(CameraLayout, Vec<String>)> {
    let camera = Camera::read(game, &layout.camera)?;
    let mut found = layout.camera.clone();
    let mut notes = vec![];
    if !camera.is_view(found.view) {
        match camera.find_view() {
            Some(view) => {
                notes.push(moved("camera.view", found.view, view, "the inverse of a camera-to-world matrix"));
                found.view = view;
            }
            None => notes.push(format!("{:<34} UNSURE, it isn't a world-to-view matrix, and no one other is", "camera.view")),
        }
    }
    if !camera.is_to_world(found.sin) {
        match camera.find_to_world() {
            Some(sin) => {
                notes.push(moved("camera.sin", found.sin, sin, "the first camera-to-world matrix"));
                notes.push(moved("camera.cos", found.cos, sin + 4, "after camera.sin"));
                found.sin = sin;
                found.cos = sin + 4;
            }
            None => notes.push(format!("{:<34} UNSURE, there's no camera-to-world matrix near Lara", "camera.sin")),
        }
    }
    if !camera.is_lens(&found) {
        match camera.find_lens(&found) {
            Some(lens) => {
                for (name, old, new) in [
                    ("camera.lens", found.lens, lens.lens),
                    ("camera.lens_near", found.lens_near, lens.lens_near),
                    ("camera.lens_far", found.lens_far, lens.lens_far),
                    ("camera.lens_fov", found.lens_fov, lens.lens_fov),
                    ("camera.lens_aspect", found.lens_aspect, lens.lens_aspect),
                ] {
                    if old != new {
                        notes.push(moved(name, old, new, "the fov and aspect ratio, after the near and far planes"));
                    }
                }
                found = lens;
            }
            None => notes.push(format!("{:<34} UNSURE, there's no fov and aspect ratio", "camera.lens")),
        }
    }
    Some((found, notes))
}

/// The layout with the camera's fields where they have to be, and the camera's chains (`CameraView`, `CameraLens`,
/// `CameraSin`, `CameraCos`: GameRender, then the field) moved with them
pub fn apply(
    mut layout: Layout,
    game: &Game,
    chains: &mut std::collections::BTreeMap<AddressType, (Vec<usize>, String)>,
    notes: &mut Vec<String>,
) -> Layout {
    let (camera, found_notes) = match derive(game, &layout) {
        Some(found) => found,
        None => return layout,
    };
    notes.extend(found_notes);
    for (address_type, offset) in [
        (AddressType::CameraView, camera.view),
        (AddressType::CameraLens, camera.lens),
        (AddressType::CameraSin, camera.sin),
        (AddressType::CameraCos, camera.cos),
    ] {
        if let Some((chain, how)) = chains.get_mut(&address_type) {
            if chain.len() == 2 && chain[1] != offset {
                chain[1] = offset;
                *how = format!("{}, camera field moved", how);
            }
        }
    }
    layout.camera = camera;
    layout
}

/// `debug camera-fields [--layout-of <version>]`: the running game's camera fields, starting from its own layout or
/// another version's
pub fn print_camera_fields(version: Option<String>, layout_of: Option<&str>) -> Result<()> {
    let connection = crate::chains::connect(version)?;
    let details = &connection.details;
    let start = match layout_of {
        Some(other) => crate::offsets_file::known_process_details()
            .into_iter()
            .find(|d| d.version.version == other)
            .with_context(|| format!("no version {} in the offsets file", other))?
            .layout,
        None => details.layout.clone(),
    };
    let game = connection.game();
    let (camera, notes) = derive(&game, &Layout { camera: start.camera.clone(), ..details.layout.clone() })
        .context("GameRender or Lara's position couldn't be read")?;
    for note in &notes {
        println!("  {}", note);
    }
    let known = &details.layout.camera;
    let pairs = [
        ("view", camera.view, known.view),
        ("lens", camera.lens, known.lens),
        ("lens_near", camera.lens_near, known.lens_near),
        ("lens_far", camera.lens_far, known.lens_far),
        ("lens_fov", camera.lens_fov, known.lens_fov),
        ("lens_aspect", camera.lens_aspect, known.lens_aspect),
        ("sin", camera.sin, known.sin),
        ("cos", camera.cos, known.cos),
    ];
    let wrong: Vec<String> = pairs.iter().filter(|(_, a, b)| a != b).map(|(n, a, b)| format!("{} {:#X} (it's {:#X})", n, a, b)).collect();
    if wrong.is_empty() {
        println!("Every camera field is where {}'s layout has it", details.version.version);
    } else {
        println!("Not where {}'s layout has them: {}", details.version.version, wrong.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A camera-to-world matrix far from the origin (as TR2013's ~30000 are), and its world-to-view inverse
    fn camera_at(position: [f32; 3]) -> ([f32; 16], [f32; 16]) {
        let (s, c) = (0.6f32, 0.8f32);
        // Turned about z, then moved, row-major for row vectors
        let to_world = [c, s, 0., 0., -s, c, 0., 0., 0., 0., 1., 0., position[0], position[1], position[2], 1.];
        let t = Vector3::new(position[0], position[1], position[2]);
        // The inverse: the transposed rotation, and the translation taken back through it
        let back = -Vector3::new(t.x * c + t.y * s, -t.x * s + t.y * c, t.z);
        let to_view = [c, -s, 0., 0., s, c, 0., 0., 0., 0., 1., 0., back.x, back.y, back.z, 1.];
        (to_world, to_view)
    }

    #[test]
    fn inverses_are_recognised_far_from_the_origin() {
        let (to_world, to_view) = camera_at([-9640.4375, -28712.3, -6458.1]);
        assert!(inverses(&to_world, &to_view));
        assert!(inverses(&to_view, &to_world));
        let (_, elsewhere) = camera_at([-9640.4375, -28712.3, -6400.0]);
        assert!(!inverses(&to_world, &elsewhere));
    }

    #[test]
    fn transforms_have_a_rotation_and_a_last_column_of_0001() {
        let (to_world, _) = camera_at([1., 2., 3.]);
        let bytes: Vec<u8> = to_world.iter().flat_map(|f| f.to_le_bytes()).collect();
        assert!(transform(&bytes, 0).is_some());
        let mut scaled = to_world;
        scaled[0] *= 2.0;
        let bytes: Vec<u8> = scaled.iter().flat_map(|f| f.to_le_bytes()).collect();
        assert!(transform(&bytes, 0).is_none());
    }
}
