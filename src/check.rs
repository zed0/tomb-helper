//! `tomb-helper check`: tests the overlay's offsets and layout against the running game, by reading things the way
//! the overlay does and checking the results make sense (RTTI classes, magic numbers, sizes, things being where Lara
//! is). Run it with Lara standing somewhere ordinary in a level.

use crate::game::Game;
use crate::game_memory::{Handle, read_floats, read_pointer};
use crate::chains;
use crate::collision::{self, AttachedProxy};
use crate::collision_mesh::CollisionMeshes;
use crate::components::{self, Components};
use crate::object_list::{self, Instance, ObjectList};
use crate::position::Position;
use crate::process_details::{AddressType, ProcessDetails};
use crate::trigger::{self, Shape};
use crate::trigger_scripts::SetOffBy;
use crate::ui_ipc_data::SurfaceKind;
use nalgebra::{Matrix3, Vector3};
use std::collections::BTreeSet;

// Things of Lara's should be this close to her
const NEAR_LARA: f32 = 300.0;
// How many of the instances nearest Lara to look at
const NEARBY: usize = 400;
const SAMPLE: usize = 64;
// Names shown of each kind of instance
const KIND_EXAMPLES: usize = 8;

#[derive(Default)]
struct Report {
    passed: usize,
    failed: usize,
    skipped: usize,
    // Everything printed, for snapshots
    lines: Vec<String>,
    options: Options,
}

/// How `check` runs
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Print each line
    pub print: bool,
    /// Scan for trigger scripts, which takes seconds
    pub scripts: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { print: true, scripts: true }
    }
}

impl Report {
    fn line(&mut self, line: String) {
        if self.options.print {
            println!("{}", line);
        }
        self.lines.push(line);
    }

    fn pass(&mut self, name: &str, detail: String) {
        self.passed += 1;
        self.line(format!("PASS  {}: {}", name, detail));
    }

    fn fail(&mut self, name: &str, detail: String) {
        self.failed += 1;
        self.line(format!("FAIL  {}: {}", name, detail));
    }

    fn skip(&mut self, name: &str, detail: String) {
        self.skipped += 1;
        self.line(format!("SKIP  {}: {}", name, detail));
    }

    fn result(&mut self, name: &str, ok: bool, detail: String) {
        if ok {
            self.pass(name, detail)
        } else {
            self.fail(name, detail)
        }
    }
}


pub fn check(version: Option<String>) -> Result<(), String> {
    let (pid, handle, base_address, details) = chains::connect(version)?;
    println!("Found {} {}", details.name, details.version.version);
    run(Handle::Live(pid, handle), base_address, details)
}

/// Checks the offsets and layout in `details` against the game
pub fn run(handle: Handle, base_address: usize, details: ProcessDetails) -> Result<(), String> {
    let (_, result) = run_lines(handle, base_address, details);
    result
}

/// `run`, and what it printed
pub fn run_lines(handle: Handle, base_address: usize, details: ProcessDetails) -> (Vec<String>, Result<(), String>) {
    run_with(handle, base_address, details, Options::default())
}

/// `run_lines`, choosing what it does
pub fn run_with(handle: Handle, base_address: usize, details: ProcessDetails, options: Options) -> (Vec<String>, Result<(), String>) {
    let mut report = Report { options, ..Default::default() };
    let result = run_report(handle, base_address, details, &mut report);
    (report.lines, result)
}

fn run_report(handle: Handle, base_address: usize, details: ProcessDetails, report: &mut Report) -> Result<(), String> {
    crate::layout::set_for(&details);
    let game = Game::new(handle, base_address, &details.address_offsets);
    let mut components = Components::new(base_address);
    // So that every component type near Lara has its slot learnt
    components.allow_scans(NEARBY * 4);

    let lara_position = check_position(&game, report);
    let instances = check_instances(&game, report);
    let nearby: Vec<Instance> = match (&lara_position, &instances) {
        (Some(lara), Some(instances)) => {
            let mut nearby = instances.clone();
            nearby.sort_by(|a, b| distance(&a.position, lara).total_cmp(&distance(&b.position, lara)));
            nearby.truncate(NEARBY);
            nearby
        }
        _ => vec![],
    };
    let lara = lara_position.as_ref().and_then(|p| check_lara(&game, report, &mut components, &nearby, p));

    check_bounds(&game, report, &mut components, &nearby);
    check_camera(&game, report);
    // Each needs something of Lara's (her instance, or offsets the game might not have), and says if it's missing
    match &lara_position {
        Some(position) => {
            check_probes(&game, report, position);
            check_proxy(&game, report, &mut components, lara, position);
            check_combat_proxy(&game, report, position);
            check_aim_sweep(&game, report, &mut components, lara, position);
            match lara {
                Some(lara) => check_capsules(&game, report, &mut components, lara, position),
                None => report.skip("hit capsules", "Lara wasn't found".to_string()),
            }
            check_collision_meshes(&game, report, position);
        }
        None => report.skip("Lara's collision and the level's", "her position couldn't be read".to_string()),
    }
    check_kinds(&game, report, &mut components, &nearby);
    match &instances {
        Some(instances) if report.options.scripts => check_trigger_scripts(&game, report, &mut components, instances, &nearby),
        _ => {}
    }

    let mut slots: Vec<_> = components.known_slots().iter().collect();
    // By class too, as some share a slot (it's a HashMap, so the report would change from run to run)
    slots.sort_by_key(|&(class, slot)| (*slot, class));
    report.line(String::new());
    report.line(format!("Component slots found: {}", slots.iter().map(|(c, s)| format!("{} {}", s, c)).collect::<Vec<_>>().join(", ")));
    report.line(String::new());
    report.line(format!("{} passed, {} failed, {} skipped", report.passed, report.failed, report.skipped));
    if report.failed > 0 {
        return Err(format!("{} checks failed", report.failed));
    }
    Ok(())
}

fn check_position(game: &Game, report: &mut Report) -> Option<Position> {
    let read = |t| game.resolve(t).and_then(|a| read_floats::<1>(game.handle, a)).map(|f| f[0]);
    match (read(AddressType::XPosition), read(AddressType::YPosition), read(AddressType::ZPosition)) {
        (Some(x), Some(y), Some(z)) if [x, y, z].iter().all(|v| v.is_finite()) => {
            report.pass("position", format!("Lara is at ({:.0}, {:.0}, {:.0})", x, y, z));
            Some(Position { x, y, z })
        }
        other => {
            report.fail("position", format!("X/Y/ZPosition don't resolve to numbers: {:?}", other));
            None
        }
    }
}

fn check_instances(game: &Game, report: &mut Report) -> Option<Vec<Instance>> {
    let array = match game.resolve(AddressType::InstanceArray) {
        Some(a) => a,
        None => {
            report.fail("instance array", "InstanceArray doesn't resolve".to_string());
            return None;
        }
    };
    let instances = match ObjectList::new(array).instances(game.handle) {
        Ok(i) if !i.is_empty() => i,
        Ok(_) => {
            report.fail("instance array", "no used slots (is the header or transform offset right?)".to_string());
            return None;
        }
        Err(e) => {
            report.fail("instance array", format!("couldn't be read: {}", e));
            return None;
        }
    };

    let layout = crate::layout::current();
    let mut classes = crate::rtti::Classes::new(game.base_address);
    let sample: Vec<&Instance> = instances.iter().step_by((instances.len() / SAMPLE).max(1)).collect();
    let transforms = sample
        .iter()
        .filter(|i| {
            layout
                .instance
                .transform
                .and_then(|offset| read_pointer(game.handle, i.address + offset))
                .and_then(|t| classes.name(game.handle, t))
                .as_deref()
                == Some(components::TRANSFORM)
        })
        .count();
    match layout.instance.transform {
        Some(_) => report.result(
            "instance array",
            transforms == sample.len(),
            format!("{} used slots, {}/{} sampled transforms are TransformComponents", instances.len(), transforms, sample.len()),
        ),
        // Instances that don't point at a transform (TR2013) are only checked by what follows
        None => report.pass("instance array", format!("{} used slots", instances.len())),
    }

    let printable = |s: &String| !s.is_empty() && s.chars().all(|c| c.is_ascii_graphic());
    let placed: Vec<&Instance> = instances.iter().filter(|i| !i.full_name.is_empty()).collect();
    let spawned = instances.len() - placed.len();
    let good_names = instances.iter().filter(|i| printable(&i.name)).count();
    // SOTTR's placements have no names, so everything goes by its object type
    report.result(
        "names",
        good_names * 10 >= instances.len() * 9,
        format!(
            "{}/{} readable ({} with placement names, {} by object type), e.g. {}",
            good_names,
            instances.len(),
            placed.len(),
            spawned,
            instances.iter().find(|i| printable(&i.name)).map_or("?", |i| i.name.as_str())
        ),
    );

    // A few spawned things can have none (TR2013's arrows), but scripts find the rest by them
    let with_id: Vec<u64> = instances.iter().map(|i| i.id).filter(|&id| id != 0).collect();
    let ids: std::collections::HashSet<u64> = with_id.iter().copied().collect();
    let without = instances.len() - with_id.len();
    report.result(
        "instance ids",
        ids.len() == with_id.len() && without * 20 <= instances.len(),
        format!("{} distinct ids for {} instances, {} without one", ids.len(), with_id.len(), without),
    );

    let rotations = sample
        .iter()
        .filter_map(|i| object_list::read_transform(game.handle, i.address).ok())
        .filter(|m| is_rotation(m, true))
        .count();
    report.result(
        "transforms",
        rotations * 10 >= sample.len() * 9,
        format!("{}/{} sampled instances' rotations (with their scale) are rotations", rotations, sample.len()),
    );
    Some(instances)
}

fn check_lara(
    game: &Game,
    report: &mut Report,
    components: &mut Components,
    nearby: &[Instance],
    position: &Position,
) -> Option<usize> {
    if crate::layout::current().instance.components.is_none() {
        report.skip("Lara", "instances have no component table to find her PlayerComponent in".to_string());
        return None;
    }
    let nearest: Vec<usize> = nearby.iter().map(|i| i.address).collect();
    let lara = game.find_lara(components, &nearest).and_then(|address| nearby.iter().find(|i| i.address == address));
    match lara {
        Some(lara) => {
            let d = distance(&lara.position, position);
            report.result(
                "Lara",
                d < 1.0,
                format!("{} has a PlayerComponent, {:.1} from the position offsets", lara.name, d),
            );
            Some(lara.address)
        }
        None => {
            report.fail("Lara", "no instance near the position has a PlayerComponent".to_string());
            None
        }
    }
}

fn check_bounds(game: &Game, report: &mut Report, components: &mut Components, nearby: &[Instance]) {
    // They're found through the instance's DrawableComponent
    if crate::layout::current().instance.components.is_none() {
        return report.skip("model bounds", "instances have no component table".to_string());
    }
    let bounds: Vec<(Position, Position)> = nearby
        .iter()
        .take(SAMPLE * 2)
        .filter_map(|i| object_list::read_bounds(game.handle, components, i.address))
        .collect();
    let sane = bounds
        .iter()
        .filter(|(min, max)| min.x <= max.x && min.y <= max.y && min.z <= max.z && max.x - min.x < 1e5)
        .count();
    report.result(
        "model bounds",
        !bounds.is_empty() && sane == bounds.len(),
        format!("{} of the nearest {} instances have a Mesh, {} with sane bounds", bounds.len(), SAMPLE * 2, sane),
    );
}

fn check_camera(game: &Game, report: &mut Report) {
    match game.resolve(AddressType::CameraView).and_then(|a| read_floats::<16>(game.handle, a)) {
        Some(view) => report.result(
            "camera view",
            is_rotation(&view, false),
            format!("rotation rows {:?}", rows(&view).map(|r| format!("{:.2}", r.norm()))),
        ),
        None => report.fail("camera view", "CameraView doesn't resolve".to_string()),
    }
    match game.resolve(AddressType::CameraLens).and_then(|a| read_floats::<8>(game.handle, a)) {
        Some(lens) => {
            let camera = &crate::layout::current().camera;
            let field = |i: usize| lens[i.min(lens.len() - 1)];
            let (near, far, fov, aspect) =
                (field(camera.lens_near), field(camera.lens_far), field(camera.lens_fov), field(camera.lens_aspect));
            report.result(
                "camera lens",
                near > 0.0 && far > near && fov > 0.1 && fov < 3.0 && aspect > 0.3 && aspect < 5.0,
                format!("near {:.1}, far {:.0}, fov {:.2}, aspect {:.2}", near, far, fov, aspect),
            );
        }
        None => report.fail("camera lens", "CameraLens doesn't resolve".to_string()),
    }
}

fn check_probes(game: &Game, report: &mut Report, lara: &Position) {
    let probes = match game.probes() {
        Some(probes) => probes,
        None => return report.skip("ground probes", "no PlayerProbes offsets".to_string()),
    };
    match probes {
        Ok(probes) => {
            // Other characters' are in the same list
            let near = collision::probes_near(&probes, lara).len();
            report.result(
                "ground probes",
                near > 0,
                format!("{} probes, {} starting near Lara", probes.len(), near),
            );
        }
        Err(e) => report.fail("ground probes", format!("couldn't be read: {}", e)),
    }
}

// The combat proxy, where it's found on its own (TR2013): it should be near Lara, in front of her while she aims
fn check_combat_proxy(game: &Game, report: &mut Report, position: &Position) {
    let proxy = match game.standalone_proxies().first() {
        Some(&p) => p,
        None => return,
    };
    match collision::read_attached_proxy(game.handle, proxy) {
        Ok((capsule, collides)) => {
            let centre = midpoint(&capsule.a, &capsule.b);
            report.result(
                "combat proxy",
                capsule.radius > 5.0 && capsule.radius < 100.0 && distance(&centre, position) < NEAR_LARA * 2.0,
                format!(
                    "radius {:.1}, {:.0} from Lara, {}",
                    capsule.radius,
                    distance(&centre, position),
                    if collides { "colliding (aiming)" } else { "not colliding" }
                ),
            );
        }
        Err(e) => report.fail("combat proxy", format!("couldn't be read: {}", e)),
    }
}

// TR2013's aim sweep, while Lara has a weapon out: a thin rod from her head forward
fn check_aim_sweep(game: &Game, report: &mut Report, components: &mut Components, lara: Option<usize>, position: &Position) {
    if !(game.has(AddressType::InventoryPool) && game.has(AddressType::TypePool)) {
        return;
    }
    let proxy = match game.lara_proxy(components, lara) {
        Some(proxy) => proxy,
        None => return report.fail("aim sweep", "Lara's proxy wasn't found".to_string()),
    };
    match game.aim_sweep(proxy) {
        Some(AttachedProxy { capsule: rod, touching: pushing }) => {
            let length = distance(&rod.a, &rod.b);
            report.result(
                "aim sweep",
                length > 10.0 && length < 300.0 && distance(&rod.a, position) < NEAR_LARA,
                format!(
                    "radius {:.1}, {:.0} long, starting {:.0} from Lara{}",
                    rod.radius,
                    length,
                    distance(&rod.a, position),
                    if pushing { ", pushing her" } else { "" }
                ),
            );
        }
        None => report.skip("aim sweep", "no weapon out (aim to check it)".to_string()),
    }
}

// Lara's movement capsule, from `PlayerProxy` if the game has it, otherwise her ProxyComponent
fn check_proxy(game: &Game, report: &mut Report, components: &mut Components, lara: Option<usize>, position: &Position) {
    let proxy = match game.lara_proxy(components, lara) {
        Some(proxy) => proxy,
        None if lara.is_none() && !game.has(AddressType::PlayerProxy) => {
            return report.skip("movement capsule", "Lara wasn't found".to_string())
        }
        None => return report.fail("movement capsule", "Lara's proxy wasn't found".to_string()),
    };
    match game.lara_proxies(proxy) {
        Ok(capsules) => match capsules.shapes.first() {
            Some(shape) => {
                let centre = midpoint(&shape.a, &shape.b);
                report.result(
                    "movement capsule",
                    shape.radius > 5.0 && shape.radius < 100.0 && distance(&centre, position) < NEAR_LARA,
                    format!(
                        "radius {:.1}, {:.0} from Lara, {} attached proxies",
                        shape.radius,
                        distance(&centre, position),
                        capsules.attached.len()
                    ),
                );
            }
            None => report.fail("movement capsule", "the proxy has no shapes".to_string()),
        },
        Err(e) => report.fail("movement capsule", format!("couldn't be read: {}", e)),
    }
}

fn check_capsules(game: &Game, report: &mut Report, components: &mut Components, lara: usize, position: &Position) {
    let shapes = match collision::read_capsule_shapes(game.handle, components, lara) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => return report.fail("hit capsules", "Lara's CollideComponent has no capsules".to_string()),
        Err(e) => return report.fail("hit capsules", format!("couldn't be read: {}", e)),
    };
    match collision::place_capsules(game.handle, lara, &shapes) {
        Ok(capsules) => {
            let good = capsules
                .iter()
                .filter(|c| c.radius > 1.0 && c.radius < 60.0 && distance(&midpoint(&c.a, &c.b), position) < NEAR_LARA)
                .count();
            report.result(
                "hit capsules",
                good == capsules.len(),
                format!("{} capsules, {} sized and placed on Lara", capsules.len(), good),
            );
        }
        Err(e) => report.fail("hit capsules", format!("couldn't be placed: {}", e)),
    }
}

fn check_collision_meshes(game: &Game, report: &mut Report, lara: &Position) {
    let scene = match game.resolve(AddressType::CollisionScene) {
        Some(s) => s,
        None => return report.skip("collision meshes", "no CollisionScene offsets".to_string()),
    };
    let mut classes = crate::rtti::Classes::new(game.base_address);
    // TR2013's scene has no RTTI, so it can only be checked by what's in it
    let class = classes.name(game.handle, scene);
    if class.is_some() && class.as_deref() != Some(".?AVCollisionSceneClient@cdc@@") {
        return report.fail("collision meshes", format!("CollisionScene is a {:?}, not a CollisionSceneClient", class));
    }
    let mut meshes = CollisionMeshes::default();
    if let Err(e) = meshes.refresh(game.handle, scene, true, true, u32::MAX) {
        return report.fail("collision meshes", format!("the instance list couldn't be read: {}", e));
    }
    let triangles = meshes.triangles_near(game.handle, lara, NEAR_LARA, NEAR_LARA, usize::MAX);
    let lara_vector = lara.vector();
    // A floor that Lara's standing on (or near, if she's mid jump)
    let under = triangles.iter().filter(|t| t.kind == SurfaceKind::Floor).map(|t| {
        let corners = t.corners.map(|c| c.vector());
        let min = corners[0].inf(&corners[1]).inf(&corners[2]);
        let max = corners[0].sup(&corners[1]).sup(&corners[2]);
        (min - lara_vector).sup(&(lara_vector - max)).sup(&Vector3::zeros()).norm()
    });
    let nearest_floor = under.fold(f32::MAX, f32::min);
    report.result(
        "collision meshes",
        !triangles.is_empty() && nearest_floor < 50.0,
        format!("{} triangles within {:.0} of Lara, the nearest floor {:.1} from her", triangles.len(), NEAR_LARA, nearest_floor),
    );
}

fn check_kinds(game: &Game, report: &mut Report, components: &mut Components, nearby: &[Instance]) {
    let (mut interactables, mut npcs, mut triggers, mut shapes) = (0, 0, 0, 0);
    // A few of each, to see they make sense
    let (mut interactable_names, mut npc_names) = (BTreeSet::new(), BTreeSet::new());
    for instance in nearby {
        let kinds = trigger::read_kinds(game.handle, components, instance.address);
        interactables += kinds.interactable as usize;
        npcs += kinds.npc as usize;
        if kinds.interactable && interactable_names.len() < KIND_EXAMPLES {
            interactable_names.insert(instance.name.clone());
        }
        if kinds.npc && npc_names.len() < KIND_EXAMPLES {
            npc_names.insert(instance.name.clone());
        }
        if kinds.trigger {
            triggers += 1;
            if let Some(shape) = trigger::read_shape(game.handle, components, instance.address) {
                let sizes_ok = match shape {
                    Shape::Box(half) => half.iter().all(|v| *v > 0.0),
                    Shape::Plane(x, y) => x > 0.0 && y > 0.0,
                    Shape::Sphere(r) => r > 0.0,
                    Shape::Poly(points) => points.len() >= 3,
                };
                shapes += sizes_ok as usize;
            }
        }
    }
    report.line(format!(
        "INFO  of the nearest {} instances: {} interactable, {} characters/animals, {} triggers",
        nearby.len(),
        interactables,
        npcs,
        triggers
    ));
    let examples = |names: &BTreeSet<String>| names.iter().cloned().collect::<Vec<_>>().join(", ");
    report.line(format!("      interactable e.g. {}", examples(&interactable_names)));
    report.line(format!("      characters/animals e.g. {}", examples(&npc_names)));
    if triggers == 0 {
        report.skip("trigger shapes", "no triggers near Lara".to_string());
    } else {
        report.result(
            "trigger shapes",
            shapes * 10 >= triggers * 8,
            format!("{}/{} triggers have a box, plane, sphere or polygon of a sane size", shapes, triggers),
        );
    }
}

fn check_trigger_scripts(
    game: &Game,
    report: &mut Report,
    components: &mut Components,
    instances: &[Instance],
    nearby: &[Instance],
) {
    let triggers: Vec<u64> = nearby
        .iter()
        .filter(|i| trigger::read_kinds(game.handle, components, i.address).trigger)
        .map(|i| i.id)
        .collect();
    if triggers.is_empty() {
        return report.skip("trigger scripts", "no triggers near Lara".to_string());
    }
    let names = instances.iter().map(|i| (i.id, i.name.clone())).collect();
    match crate::trigger_scripts::scripts_now(game.handle, game.base_address, &triggers, &names) {
        Ok(found) => {
            let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
            let mut does: std::collections::BTreeMap<String, usize> = Default::default();
            for script in found.values() {
                for what in &script.does {
                    *does.entry(what.clone()).or_insert(0) += 1;
                }
            }
            for set_off_by in found.values().map(|s| &s.set_off_by) {
                let key = match set_off_by {
                    SetOffBy::Lara => "Lara".to_string(),
                    SetOffBy::Instance(Some(name)) => name.clone(),
                    SetOffBy::Instance(None) => "an instance".to_string(),
                    SetOffBy::Script => "other scripts".to_string(),
                    SetOffBy::Nothing => "no script".to_string(),
                };
                *counts.entry(key).or_insert(0) += 1;
            }
            let with_script = found.values().filter(|s| s.set_off_by != SetOffBy::Nothing).count();
            // Most of SOTTR's village's are used by a script, but only about a third of TR2013's (its effect volumes and
            // many plain ones aren't used by any) or ROTTR's valley hub's (NPC proximity checks and assigned locations:
            // nothing in memory but their own components has their ids)
            report.result(
                "trigger scripts",
                with_script * 4 >= found.len(),
                format!("{}/{} triggers near Lara have a script: {:?}", with_script, found.len(), counts),
            );
            // The graph layout is right if the links lead somewhere
            let described = found.values().filter(|s| !s.does.is_empty()).count();
            report.result(
                "trigger script links",
                described > 0,
                format!("{}/{} triggers near Lara do something that's known: {:?}", described, with_script, does),
            );
        }
        Err(e) => report.fail("trigger scripts", e),
    }
}

// The 3x3 part's rows are unit length (or all the same length, if `scaled`) and at right angles
fn is_rotation(matrix: &[f32; 16], scaled: bool) -> bool {
    let rows = rows(matrix);
    let scale = rows[0].norm();
    if !(scale > 1e-3) || (!scaled && (scale - 1.0).abs() > 0.05) {
        return false;
    }
    let m = Matrix3::from_rows(&[rows[0].transpose(), rows[1].transpose(), rows[2].transpose()]) / scale;
    (m * m.transpose() - Matrix3::identity()).abs().max() < 0.05
}

fn rows(matrix: &[f32; 16]) -> [Vector3<f32>; 3] {
    [0, 4, 8].map(|i| Vector3::new(matrix[i], matrix[i + 1], matrix[i + 2]))
}

fn distance(a: &Position, b: &Position) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

fn midpoint(a: &Position, b: &Position) -> Position {
    Position { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0, z: (a.z + b.z) / 2.0 }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations_are_told_apart_from_other_matrices() {
        let identity = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 5., 6., 7., 1.];
        assert!(is_rotation(&identity, false));
        let scaled = identity.map(|v| v * 2.0);
        assert!(!is_rotation(&scaled, false));
        assert!(is_rotation(&scaled, true));
        let skewed = [1., 0.5, 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
        assert!(!is_rotation(&skewed, true));
    }
}
