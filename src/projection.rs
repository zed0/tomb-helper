//! Projecting the world onto the screen, shared by the overlay (drawing) and the exe (choosing which of the level's
//! collision triangles the overlay draws, `visible_triangles`, so the overlay doesn't do that on the game's render thread
//! every frame): the camera's `Projector`, clipping at the near plane, fading with distance, and `OcclusionBuffer`, a low
//! resolution software ID buffer of which triangle is nearest the camera at each pixel.

use crate::ui_ipc_data::{Triangle, UiSettings};
use nalgebra::{Matrix4, Vector3, Vector4};

// Points closer to the camera plane than this (in clip space w) are clipped off
pub const NEAR_CLIP_W: f32 = 1.0;
// Anything faded more than this isn't drawn
pub const MIN_OPACITY: f32 = 0.005;
// When hiding triangles behind others, they're rasterised into a buffer this many times smaller than the screen,
// keeping the nearest at each pixel, and only those nearest somewhere are drawn. Smaller triangles can be missed.
pub const OCCLUSION_SCALE: f32 = 4.0;

// The game stores matrices row-major for row vectors (`v * M`). Reading the same floats column-major
// gives the transposed matrix, which is the equivalent for nalgebra's column vectors (`M * v`).
pub fn game_matrix(elements: &[f32; 16]) -> Matrix4<f32> {
    Matrix4::from_column_slice(elements)
}

// The part of the convex polygon (in clip space) in front of the near plane
pub fn clip_polygon(corners: &[Vector4<f32>]) -> Vec<Vector4<f32>> {
    let mut clipped = vec![];
    for (i, &a) in corners.iter().enumerate() {
        let b = corners[(i + 1) % corners.len()];
        if a.w >= NEAR_CLIP_W {
            clipped.push(a);
        }
        if (a.w >= NEAR_CLIP_W) != (b.w >= NEAR_CLIP_W) {
            clipped.push(near_plane_crossing(a, b));
        }
    }
    clipped
}

// Where the line from `a` to `b` (in clip space) crosses the near plane
fn near_plane_crossing(a: Vector4<f32>, b: Vector4<f32>) -> Vector4<f32> {
    a + (b - a) * ((a.w - NEAR_CLIP_W) / (a.w - b.w))
}

pub struct Projector {
    pub view_projection: Matrix4<f32>,
    pub camera_position: Vector3<f32>,
    pub screen_width: f32,
    pub screen_height: f32,
}

impl Projector {
    // From the game's row-major world-to-view and projection matrices
    pub fn new(view: &[f32; 16], projection: &[f32; 16], screen: [f32; 2]) -> Projector {
        let view = game_matrix(view);
        Projector {
            view_projection: game_matrix(projection) * view,
            camera_position: view.try_inverse().map_or(Vector3::zeros(), |m| m.column(3).xyz()),
            screen_width: screen[0],
            screen_height: screen[1],
        }
    }

    // Seeing `margin` (a fraction of the screen's width and height) further off each side of the screen, at the same
    // scale, for choosing what to draw from a camera that lags behind the one it's drawn with
    pub fn widened(&self, margin: f32) -> Projector {
        let scale = 1.0 + 2.0 * margin;
        let shrink = Matrix4::from_diagonal(&Vector4::new(1.0 / scale, 1.0 / scale, 1.0, 1.0));
        Projector {
            view_projection: shrink * self.view_projection,
            camera_position: self.camera_position,
            screen_width: self.screen_width * scale,
            screen_height: self.screen_height * scale,
        }
    }

    pub fn to_clip(&self, model: &Matrix4<f32>, point: Vector3<f32>) -> Vector4<f32> {
        self.view_projection * model * point.push(1.0)
    }

    pub fn world_to_clip(&self, point: Vector3<f32>) -> Vector4<f32> {
        self.view_projection * point.push(1.0)
    }

    pub fn to_screen(&self, clip: Vector4<f32>) -> [f32; 2] {
        let ndc = clip.xy() / clip.w;
        // The game's view space has y pointing down the screen, so there's no flip here
        [
            (ndc.x + 1.0) * 0.5 * self.screen_width,
            (ndc.y + 1.0) * 0.5 * self.screen_height,
        ]
    }

    pub fn project(&self, point: Vector3<f32>) -> Option<[f32; 2]> {
        let clip = self.world_to_clip(point);
        if clip.w < NEAR_CLIP_W {
            return None;
        }
        Some(self.to_screen(clip))
    }

    // Clips the convex polygon against the near plane, returns its corners on screen (none if it's entirely behind
    // the camera)
    pub fn project_polygon(&self, corners: &[Vector4<f32>]) -> Vec<[f32; 2]> {
        clip_polygon(corners).into_iter().map(|c| self.to_screen(c)).collect()
    }

    // Clips the line against the near plane, returns None if it's entirely behind the camera
    pub fn project_line(&self, a: Vector4<f32>, b: Vector4<f32>) -> Option<([f32; 2], [f32; 2])> {
        if a.w < NEAR_CLIP_W && b.w < NEAR_CLIP_W {
            return None;
        }
        let a2 = if a.w < NEAR_CLIP_W { near_plane_crossing(b, a) } else { a };
        let b2 = if b.w < NEAR_CLIP_W { near_plane_crossing(a, b) } else { b };
        Some((self.to_screen(a2), self.to_screen(b2)))
    }
}

// How much fainter to draw something this far from where it's drawn round, by (1 - distance / radius) ^ strength, if
// fading with distance is on
pub fn fade(settings: &UiSettings, distance: f32, radius: f32) -> f32 {
    if !settings.fade_with_distance.get() {
        return 1.0;
    }
    let t = (distance / radius.max(1.0)).min(1.0);
    (1.0 - t).powf(settings.fade_strength.get())
}

// How far from where they're drawn round triangles of each kind ([coarse, detailed]) are drawn: their radius, or less
// where fading would leave them too faint to see. None for kinds that aren't drawn.
pub fn mesh_reach(settings: &UiSettings) -> [Option<f32>; 2] {
    let collision = settings.collision.get();
    let opacity = settings.mesh_opacity.get();
    let reach = |shown: bool, radius: f32| {
        if !shown || opacity < MIN_OPACITY {
            return None;
        }
        if !settings.fade_with_distance.get() {
            return Some(radius);
        }
        // Where opacity * (1 - distance / radius) ^ strength is MIN_OPACITY
        let radius = radius.max(1.0);
        Some(radius * (1.0 - (MIN_OPACITY / opacity).powf(1.0 / settings.fade_strength.get().max(0.01))))
    };
    [
        reach(collision && settings.coarse_meshes.get(), settings.coarse_radius.get()),
        reach(collision && settings.detailed_meshes.get(), settings.detailed_radius.get()),
    ]
}

// How far the point is from the nearest part of the box round the points
pub fn box_distance(point: &Vector3<f32>, points: &[Vector3<f32>]) -> f32 {
    let (min, max) = points.iter().fold((points[0], points[0]), |(min, max), p| (min.inf(p), max.sup(p)));
    (min - point).sup(&(point - max)).sup(&Vector3::zeros()).norm()
}

// How far the point is from the nearest part of the box round the triangle, like the exe's choice of triangles
pub fn triangle_distance(point: &Vector3<f32>, triangle: &Triangle) -> f32 {
    box_distance(point, &triangle.corners.map(|c| c.vector()))
}

// The triangle's corners in clip space, unless it's seen from behind or can't be on screen
pub fn triangle_on_screen(projector: &Projector, triangle: &Triangle) -> Option<[Vector4<f32>; 3]> {
    let world = triangle.corners.map(|c| c.vector());
    // Triangles are wound anticlockwise seen from the side they face
    let normal = (world[1] - world[0]).cross(&(world[2] - world[0]));
    if normal.dot(&(projector.camera_position - world[0])) <= 0.0 {
        return None;
    }
    face_on_screen(projector, world)
}

// The corners in clip space, unless they can't be on screen
pub fn face_on_screen(projector: &Projector, world: [Vector3<f32>; 3]) -> Option<[Vector4<f32>; 3]> {
    let corners = world.map(|c| projector.world_to_clip(c));
    // All off the same side of the screen, or behind the camera
    let outside = |test: fn(&Vector4<f32>) -> bool| corners.iter().all(test);
    if outside(|c| c.x < -c.w)
        || outside(|c| c.x > c.w)
        || outside(|c| c.y < -c.w)
        || outside(|c| c.y > c.w)
        || outside(|c| c.w < NEAR_CLIP_W)
    {
        return None;
    }
    Some(corners)
}

// A collision triangle's distance from where they're drawn round and its corners in clip space, if it's drawn: its kind
// is shown, it's within its reach (`mesh_reach`), it faces the camera and it's on screen
pub fn drawn_triangle(
    projector: &Projector,
    triangle: &Triangle,
    centre: &Vector3<f32>,
    reach: &[Option<f32>; 2],
) -> Option<(f32, [Vector4<f32>; 3])> {
    let reach = reach[triangle.detailed as usize]?;
    let distance = triangle_distance(centre, triangle);
    if distance > reach {
        return None;
    }
    Some((distance, triangle_on_screen(projector, triangle)?))
}

/// Which of the collision triangles the overlay should draw (`drawn_triangle`), and unless `cull_hidden` only those
/// nearest the camera somewhere on screen: their indices, in order
pub fn visible_triangles(
    projector: &Projector,
    triangles: &[Triangle],
    centre: &Vector3<f32>,
    reach: &[Option<f32>; 2],
    cull_hidden: bool,
    occlusion: &mut OcclusionBuffer,
) -> Vec<usize> {
    let drawn: Vec<(usize, [Vector4<f32>; 3])> = triangles
        .iter()
        .enumerate()
        .filter_map(|(i, t)| drawn_triangle(projector, t, centre, reach).map(|(_, corners)| (i, corners)))
        .collect();
    if !cull_hidden {
        return drawn.into_iter().map(|(i, _)| i).collect();
    }
    occlusion.reset(projector.screen_width, projector.screen_height);
    for (id, (_, corners)) in drawn.iter().enumerate() {
        occlusion.add(id as u32, projector, corners);
    }
    let visible = occlusion.visible(drawn.len());
    drawn.into_iter().zip(visible).filter(|(_, visible)| *visible).map(|((i, _), _)| i).collect()
}

// Keeps which triangle is nearest the camera at each pixel, at a lower resolution than the screen
#[derive(Default)]
pub struct OcclusionBuffer {
    width: usize,
    height: usize,
    // 1 / w, which is linear across the screen, so bigger is nearer
    depth: Vec<f32>,
    ids: Vec<u32>,
}

impl OcclusionBuffer {
    pub fn reset(&mut self, screen_width: f32, screen_height: f32) {
        self.width = (screen_width / OCCLUSION_SCALE).ceil().max(1.0) as usize;
        self.height = (screen_height / OCCLUSION_SCALE).ceil().max(1.0) as usize;
        self.depth.clear();
        self.depth.resize(self.width * self.height, 0.0);
        self.ids.clear();
        self.ids.resize(self.width * self.height, u32::MAX);
    }

    // `corners` are in clip space
    pub fn add(&mut self, id: u32, projector: &Projector, corners: &[Vector4<f32>; 3]) {
        let points: Vec<[f32; 3]> = clip_polygon(corners)
            .into_iter()
            .map(|c| {
                let [x, y] = projector.to_screen(c);
                [x / OCCLUSION_SCALE, y / OCCLUSION_SCALE, 1.0 / c.w]
            })
            .collect();
        for i in 2..points.len() {
            self.rasterise(id, [points[0], points[i - 1], points[i]]);
        }
    }

    // Each pixel whose centre is inside the triangle (corners x, y in pixels, then depth), a row at a time: each
    // corner's barycentric weight is linear across the buffer, so a row's pixels with all three at least 0 are a span
    // that can be worked out directly, and so is the depth along it
    fn rasterise(&mut self, id: u32, p: [[f32; 3]; 3]) {
        let edge = |a: [f32; 3], b: [f32; 3], x: f32, y: f32| (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
        let area = edge(p[0], p[1], p[2][0], p[2][1]);
        if area.abs() < 1e-6 {
            return;
        }
        // The weight of the corner opposite the edge from a to b as (constant, per x, per y), positive inside whichever
        // way it's wound
        let weight = |a: [f32; 3], b: [f32; 3]| {
            let (per_x, per_y) = (-(b[1] - a[1]) / area, (b[0] - a[0]) / area);
            [-(per_x * a[0] + per_y * a[1]), per_x, per_y]
        };
        let weights = [weight(p[1], p[2]), weight(p[2], p[0]), weight(p[0], p[1])];
        let depth: [f32; 3] = std::array::from_fn(|k| (0..3).map(|i| weights[i][k] * p[i][2]).sum());

        let min_x = p.iter().map(|c| c[0]).fold(f32::MAX, f32::min).floor().max(0.0);
        let max_x = p.iter().map(|c| c[0]).fold(f32::MIN, f32::max).ceil().min(self.width as f32 - 1.0);
        let min_y = p.iter().map(|c| c[1]).fold(f32::MAX, f32::min).floor().max(0.0) as i64;
        let max_y = p.iter().map(|c| c[1]).fold(f32::MIN, f32::max).ceil().min(self.height as f32 - 1.0) as i64;
        'rows: for y in min_y..=max_y {
            let py = y as f32 + 0.5;
            // The pixel centres (x + 0.5) where every weight is at least 0
            let (mut low, mut high) = (min_x + 0.5, max_x + 0.5);
            for [constant, per_x, per_y] in weights {
                let at_x0 = constant + per_y * py;
                if per_x > 0.0 {
                    low = low.max(-at_x0 / per_x);
                } else if per_x < 0.0 {
                    high = high.min(-at_x0 / per_x);
                } else if at_x0 < 0.0 {
                    continue 'rows;
                }
            }
            let row_depth = depth[0] + depth[2] * py;
            let row = y as usize * self.width;
            for x in (low - 0.5).ceil() as i64..=(high - 0.5).floor() as i64 {
                let depth = row_depth + depth[1] * (x as f32 + 0.5);
                let i = row + x as usize;
                if depth > self.depth[i] {
                    self.depth[i] = depth;
                    self.ids[i] = id;
                }
            }
        }
    }

    // Which of the first `count` ids are nearest at some pixel
    pub fn visible(&self, count: usize) -> Vec<bool> {
        let mut visible = vec![false; count];
        for &id in &self.ids {
            if let Some(v) = visible.get_mut(id as usize) {
                *v = true;
            }
        }
        visible
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearer_triangles_hide_ones_behind_them() {
        let mut buffer = OcclusionBuffer::default();
        buffer.reset(40.0 * OCCLUSION_SCALE, 40.0 * OCCLUSION_SCALE);
        // Depth is 1 / w, so 0.5 is nearer than 0.1. The far one is entirely behind the near one, the other only partly.
        buffer.rasterise(0, [[5., 5., 0.1], [15., 5., 0.1], [5., 15., 0.1]]);
        buffer.rasterise(1, [[0., 0., 0.5], [30., 0., 0.5], [0., 30., 0.5]]);
        buffer.rasterise(2, [[20., 20., 0.1], [39., 20., 0.1], [20., 39., 0.1]]);
        assert_eq!(buffer.visible(3), [false, true, true]);
    }

    // The spans cover the pixels whose centres are inside, as testing each pixel does, either way round
    #[test]
    fn spans_cover_the_pixels_inside() {
        let triangles = [
            [[3.2, 1.7, 1.0], [37.9, 12.4, 2.0], [11.1, 38.6, 3.0]],
            [[30.5, 2.0, 1.0], [2.5, 20.25, 1.0], [39.0, 35.0, 1.0]],
            [[-10.0, -5.0, 1.0], [50.0, 10.0, 1.0], [20.0, 60.0, 1.0]],
            [[5.0, 5.0, 1.0], [35.0, 5.0, 1.0], [20.0, 6.0, 1.0]],
        ];
        for corners in triangles {
            for order in [[0, 1, 2], [0, 2, 1]] {
                let p = order.map(|i| corners[i]);
                let mut buffer = OcclusionBuffer::default();
                buffer.reset(40.0 * OCCLUSION_SCALE, 40.0 * OCCLUSION_SCALE);
                buffer.rasterise(0, p);
                let edge = |a: [f32; 3], b: [f32; 3], x: f32, y: f32| (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
                let area = edge(p[0], p[1], p[2][0], p[2][1]);
                let mut covered = 0;
                for y in 0..40 {
                    for x in 0..40 {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        let weights = [edge(p[1], p[2], px, py), edge(p[2], p[0], px, py), edge(p[0], p[1], px, py)]
                            .map(|e| e / area);
                        let inside = weights.iter().all(|&w| w >= 0.0);
                        let drawn = buffer.ids[y * 40 + x] == 0;
                        covered += drawn as usize;
                        // Rounding can put a centre that's right on an edge either side of it
                        let on_edge = weights.iter().any(|w| w.abs() < 1e-4);
                        assert!(inside == drawn || on_edge, "({}, {}) of {:?}", x, y, p);
                    }
                }
                assert!(covered > 0);
            }
        }
    }

    // Depth is interpolated across the triangle as before: the corners' 1 / w, weighted
    #[test]
    fn depth_is_interpolated() {
        let mut buffer = OcclusionBuffer::default();
        buffer.reset(40.0 * OCCLUSION_SCALE, 40.0 * OCCLUSION_SCALE);
        buffer.rasterise(0, [[0.0, 0.0, 1.0], [40.0, 0.0, 3.0], [0.0, 40.0, 5.0]]);
        // At (10.5, 10.5): weights 1 - 21/40, 10.5/40, 10.5/40
        let expected = (1.0 - 21.0 / 40.0) * 1.0 + 10.5 / 40.0 * 3.0 + 10.5 / 40.0 * 5.0;
        assert!((buffer.depth[10 * 40 + 10] - expected).abs() < 1e-4);
    }

    #[test]
    fn faded_meshes_reach_as_far_as_they_can_be_seen() {
        let settings: Box<UiSettings> = unsafe { Box::new(std::mem::MaybeUninit::zeroed().assume_init()) };
        settings.collision.set(true);
        settings.coarse_meshes.set(true);
        settings.coarse_radius.set(1500.0);
        settings.detailed_radius.set(1500.0);
        settings.mesh_opacity.set(0.25);
        assert_eq!(mesh_reach(&settings), [Some(1500.0), None]);
        settings.fade_with_distance.set(true);
        settings.fade_strength.set(2.0);
        let reach = mesh_reach(&settings)[0].unwrap();
        // Exactly as faint as is drawn there
        assert!((0.25 * fade(&settings, reach, 1500.0) - MIN_OPACITY).abs() < 1e-5);
        assert!(reach < 1500.0);
    }

    // Widening sees past the edges of the screen, but puts what was on it in the same place, offset by the margin
    #[test]
    fn widening_keeps_the_scale() {
        let mut view = [0.0f32; 16];
        for i in 0..4 {
            view[i * 5] = 1.0;
        }
        let projection = crate::ui_ipc_data::projection_from_lens(&[10.0, 10000.0, 1.2, 16.0 / 9.0, 0.0, 0.0, 0.0, 0.0], [0, 1, 2, 3]);
        let projector = Projector::new(&view, &projection, [1600.0, 900.0]);
        let wide = projector.widened(0.25);
        let point = Vector3::new(100.0, 50.0, 1000.0);
        let (a, b) = (projector.project(point).unwrap(), wide.project(point).unwrap());
        assert!((b[0] - (a[0] + 400.0)).abs() < 1e-2 && (b[1] - (a[1] + 225.0)).abs() < 1e-2, "{:?} {:?}", a, b);
        // Just off the screen is on the wider one
        let off = Vector3::new(-1.1 * 1000.0 * 16.0 / 9.0 / (1.0 / (0.6f32).tan()), 0.0, 1000.0);
        let x = wide.project(off).unwrap()[0];
        assert!(x > 0.0 && x < 400.0, "{}", x);
    }
}
