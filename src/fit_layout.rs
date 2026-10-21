//! `tomb-helper check --fit [<group>...] [--from <snapshot>] [--save]`: searches for layout offsets that `check` passes
//! with, for a build whose structures have changed. Fields usually move because something was added before them, which
//! moves every field after it by as much, so for each group of a failing check (or the groups named) it tries moving
//! the fields from each one on by up to `MAX_SHIFT` either way (growing the group's read sizes to match), scores each
//! try by that group's checks, and keeps the best. It runs `check` quietly and without the trigger script scan, on the
//! running game or a snapshot.
//!
//! Read sizes (`READ_SIZES`) grow with the fields; values that aren't offsets (`NOT_OFFSETS`, add new ones there) never
//! move. Panics from bad layouts are caught. It recovered a +8 shift of TR2013's proxy from a snapshot in 23s.

use anyhow::{Context, Result};
use crate::check::{self, Options};
use crate::discover;
use crate::game_memory::Handle;
use crate::layout::Layout;
use crate::layout_witness::{field_offset, with_field};
use crate::offsets_file::VersionEntry;
use crate::process_details::ProcessDetails;
use crate::snapshot::Snapshot;
use std::path::Path;

// How far a field is tried either way, and in steps of
const MAX_SHIFT: i64 = 0x40;
const STEP: i64 = 4;
// Rounds of trying every group again after one improves
const MAX_ROUNDS: usize = 3;

/// Which layout groups each check depends on
const CHECK_GROUPS: &[(&str, &[&str])] = &[
    ("instance array", &["instance"]),
    ("names", &["instance", "placement"]),
    ("instance ids", &["instance"]),
    ("transforms", &["instance", "transform"]),
    ("Lara", &["instance", "component"]),
    ("model bounds", &["drawable"]),
    ("camera view", &["camera"]),
    ("camera lens", &["camera"]),
    ("ground probes", &["probes"]),
    ("movement capsule", &["proxy", "body", "physics_shape"]),
    ("combat proxy", &["proxy"]),
    ("aim sweep", &["aim_sweep", "proxy"]),
    ("hit capsules", &["collide", "transform"]),
    ("collision meshes", &["collision_scene", "collision_instance", "collision_mesh"]),
    ("trigger shapes", &["trigger_shape", "trigger_object", "component_data"]),
];

/// Layout values that aren't offsets into a structure: how much is read (which grow when fields move on), and type
/// numbers, masks and indices (which never move)
const READ_SIZES: &[&str] = &[
    "instance.size",
    "proxy.size",
    "physics_shape.size",
    "collision_instance.size",
    "collision_instance.placement_size",
    "collision_mesh.header_size",
    "trigger_shape.record_size",
    "script.node_size",
];
const NOT_OFFSETS: &[&str] = &[
    "proxy.contact_size",
    "proxy.collides",
    "component_data.entry_size",
    "aim_sweep.max_handles",
    "aim_sweep.handle_size",
    "aim_sweep.inventory_size",
    "aim_sweep.type_size",
    "transform.bone_size",
    "collide.entry_size",
    "collide.capsule_type",
    "collide.capsule_type_mask",
    "collide.capsule_size",
    "physics_shape.capsule_type",
    "probes.size",
    "camera.lens_near",
    "camera.lens_far",
    "camera.lens_fov",
    "camera.lens_aspect",
    "collision_instance.player_kind",
    "collision_mesh.format_float",
    "collision_mesh.format_half",
    "collision_mesh.triangle_size",
    "collision_mesh.triangle_ignored",
    "trigger_shape.box_type",
    "trigger_shape.plane_type",
    "trigger_shape.sphere_type",
    "trigger_shape.poly_type",
    "trigger_shape.poly_point_size",
    "trigger_object.volume_box",
    "trigger_object.volume_sphere",
    "script.link_size",
];

// The game to check against, the version, and how to run `check` on a layout
struct Target {
    handle: Handle,
    base_address: usize,
    details: ProcessDetails,
}

impl Target {
    // What `check` says of each check with this layout: (name, passed, failed)
    fn check(&self, layout: &Layout) -> Vec<(String, bool, bool)> {
        let details = ProcessDetails { layout: layout.clone(), ..self.details.clone() };
        let options = Options { print: false, scripts: false };
        // A wrong layout can have the readers index past what they read
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check::run_with(self.handle, self.base_address, details, options).0
        }));
        run.unwrap_or_default()
            .iter()
            .filter_map(|line| {
                let (status, rest) = line.split_at(line.len().min(6));
                let name = rest.split(':').next()?.to_string();
                match status.trim() {
                    "PASS" => Some((name, true, false)),
                    "FAIL" => Some((name, false, true)),
                    "SKIP" => Some((name, false, false)),
                    _ => None,
                }
            })
            .collect()
    }
}

// How well a check run went for a group: its checks first, then everything
fn score(results: &[(String, bool, bool)], group: &str) -> (i64, i64) {
    let points = |(_, passed, failed): &(String, bool, bool)| *passed as i64 - *failed as i64;
    let relevant = results.iter().filter(|(name, _, _)| groups_of(name).contains(&group)).map(points).sum();
    (relevant, results.iter().map(points).sum())
}

fn groups_of(check: &str) -> &'static [&'static str] {
    CHECK_GROUPS.iter().find(|(name, _)| *name == check).map_or(&[], |(_, groups)| groups)
}

// The group's fields that are offsets, and its read sizes
fn fields(layout: &Layout, group: &str) -> (Vec<(String, usize)>, Vec<(String, usize)>) {
    let value = serde_json::to_value(layout).unwrap_or_default();
    let names: Vec<String> = value
        .get(group)
        .and_then(|g| g.as_object())
        .map_or(vec![], |g| g.keys().map(|k| format!("{}.{}", group, k)).collect());
    let mut offsets = vec![];
    let mut sizes = vec![];
    for name in names {
        if let Some(offset) = field_offset(layout, &name) {
            if READ_SIZES.contains(&name.as_str()) {
                sizes.push((name, offset));
            } else if !NOT_OFFSETS.contains(&name.as_str()) {
                offsets.push((name, offset));
            }
        }
    }
    (offsets, sizes)
}

// The layout with the group's fields from `from` on moved by `shift`
fn shifted(layout: &Layout, group: &str, from: usize, shift: i64) -> Option<Layout> {
    let (offsets, sizes) = fields(layout, group);
    let mut changed = layout.clone();
    for (name, offset) in offsets.iter().filter(|(_, offset)| *offset >= from) {
        let moved = *offset as i64 + shift;
        if moved < 0 {
            return None;
        }
        changed = with_field(&changed, name, moved as usize).ok()?;
    }
    if shift > 0 {
        for (name, size) in &sizes {
            changed = with_field(&changed, name, size + shift as usize).ok()?;
        }
    }
    Some(changed)
}

// The best way to move the group's fields, if any is better than how they are
fn fit_group(target: &Target, layout: &Layout, group: &str, current: (i64, i64)) -> Option<(Layout, (i64, i64), String)> {
    let (offsets, _) = fields(layout, group);
    let mut starts: Vec<usize> = offsets.iter().map(|(_, o)| *o).collect();
    starts.sort();
    starts.dedup();
    let mut best: Option<(Layout, (i64, i64), String)> = None;
    for &from in &starts {
        for shift in (-MAX_SHIFT..=MAX_SHIFT).step_by(STEP as usize).filter(|s| *s != 0) {
            let candidate = match shifted(layout, group, from, shift) {
                Some(c) => c,
                None => continue,
            };
            let points = score(&target.check(&candidate), group);
            let better = points > best.as_ref().map_or(current, |(_, p, _)| *p);
            if better {
                let how = format!("{} fields from {:#X} on moved by {}{:#X}", group, from, if shift < 0 { "-" } else { "+" }, shift.abs());
                best = Some((candidate, points, how));
            }
        }
    }
    best
}

/// `check --fit`: finds what to change in the version's layout for its checks to pass
pub fn fit_layout(groups: &[&str], snapshot: Option<&str>, version: Option<String>, save: bool) -> Result<()> {
    let target = match snapshot {
        Some(path) => {
            let snapshot = Snapshot::load(Path::new(path)).with_context(|| format!("couldn't load {}", path))?;
            let snapshot: &'static Snapshot = Box::leak(Box::new(snapshot));
            Target { handle: Handle::Snapshot(snapshot), base_address: snapshot.header.base_address, details: snapshot.details()? }
        }
        None => {
            let (pid, handle, base_address, details) = crate::chains::connect(version)?;
            Target { handle: Handle::Live(pid, handle), base_address, details }
        }
    };
    println!("Fitting the layout of {} {}", target.details.name, target.details.version.version);
    let mut layout = target.details.layout.clone();
    let started = std::time::Instant::now();
    let results = target.check(&layout);
    println!("{} checks pass as it is ({:.1}s a check)", results.iter().filter(|r| r.1).count(), started.elapsed().as_secs_f32());
    // The groups to try: those named, else those of the checks that fail
    let mut wanted: Vec<String> = groups.iter().map(|g| g.to_string()).collect();
    if wanted.is_empty() {
        for (name, _, failed) in &results {
            for group in groups_of(name).iter().filter(|_| *failed) {
                if !wanted.iter().any(|w| w == group) {
                    wanted.push(group.to_string());
                }
            }
        }
    }
    if wanted.is_empty() {
        println!("Nothing fails, so there's nothing to fit (name groups to try them anyway)");
        return Ok(());
    }
    let previous = hide_panics();
    let mut changes = vec![];
    for round in 0..MAX_ROUNDS {
        let mut improved = false;
        for group in &wanted {
            let current = score(&target.check(&layout), group);
            if let Some((better, points, how)) = fit_group(&target, &layout, group, current) {
                println!("  {} ({} -> {} for its checks)", how, current.0, points.0);
                layout = better;
                changes.push(how);
                improved = true;
            }
        }
        if !improved {
            break;
        }
        println!("Round {} done, trying again", round + 1);
    }
    std::panic::set_hook(previous);
    if changes.is_empty() {
        println!("No moves of {} made more checks pass", wanted.join(", "));
        return Ok(());
    }
    let entry = VersionEntry { layout: layout.clone(), ..VersionEntry::from_details(&target.details) };
    println!("\nWhat changes:");
    for (name, old, new) in differences(&target.details.layout, &layout) {
        println!("  {:<34} {} -> {}", name, old, new);
    }
    let results = target.check(&layout);
    println!("{} checks pass with it", results.iter().filter(|r| r.1).count());
    if save {
        discover::save_entry(entry)?;
    }
    Ok(())
}

type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

// Wrong layouts can panic the readers, which is expected while searching, so their messages are hidden
fn hide_panics() -> PanicHook {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    previous
}

// The fields that differ between two layouts, as (group.field, old, new)
fn differences(old: &Layout, new: &Layout) -> Vec<(String, String, String)> {
    let (old, new) = (serde_json::to_value(old).unwrap_or_default(), serde_json::to_value(new).unwrap_or_default());
    let mut found = vec![];
    for (group, fields) in new.as_object().into_iter().flatten() {
        for (field, value) in fields.as_object().into_iter().flatten() {
            let before = old.get(group).and_then(|g| g.get(field));
            if before != Some(value) {
                found.push((format!("{}.{}", group, field), before.map_or("?".to_string(), |b| b.to_string()), value.to_string()));
            }
        }
    }
    found
}
