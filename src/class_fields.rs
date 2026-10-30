//! How a class's own code uses its fields: each virtual function of the class (found through its RTTI vtable, which has
//! the same name in every game and build, 32 or 64-bit) is traced from its start with `this` in rcx (ecx on x86,
//! thiscall), following its calls and its own virtual calls, and the accesses through it are its fields
//! (`code_trace::trace_object`). `debug class-fields <image> <class>` prints them.
//!
//! `FIELD_CLASSES` says which class each layout field is in, so `derive_layout` (`debug class-layout <from> <to>`) can
//! work out another build's or game's layout: each virtual function's accesses are lined up with the same function's
//! there (by how deep, how big and whether each is followed, as compilers pick different instructions), and a field
//! the code uses goes where its accesses line up (`Matched`). Others are only placed when the used fields either side
//! moved by the same amount, and that's often wrong, as are single matches (`Guessed`); `check` has to confirm those.
//! Measured on the dumps (`debug class-layout`): ROTTR -> SOTTR 234.1 got every field it matched right (9, 5 of them
//! known), ROTTR -> TR2013 (another compiler, 32-bit) the proxy's flags and instance, but TR2013 Steam -> v1.01.748 said
//! the flags stayed at 0x10 when they're at 0x8 there: matches are good evidence, not proof. Only the fields the
//! virtual functions use are covered (not the proxy's shapes, or the camera's), and a field another game doesn't have
//! can be matched to one that's used the same way (ROTTR's physics body to something of TR2013's, which has none).
//! `update-offsets` uses what's matched (and within a game what's placed) for a new build's layout (`class_layout`).

use anyhow::{bail, Result};
use crate::code_trace::{Access, Tracer};
use crate::components;
use crate::derive::ChainVotes;
use crate::layout::{field_offset, FieldKind, Layout};
use crate::pe_image::PeImage;
use crate::rtti;
use iced_x86::Register;
use std::collections::BTreeMap;

// Virtual functions read from a vtable at most
const MAX_VIRTUALS: usize = 400;
// How deep calls from a virtual function are followed, and how much of it is looked at
const CALL_DEPTH: usize = 4;
const INSTRUCTIONS: usize = 3000;

/// The RVAs of the virtual functions of the class's vtables (a class with several bases has several), in vtable order
pub fn virtual_functions(image: &PeImage, class: &str) -> Vec<Vec<usize>> {
    let pointer_size = image.pointer_size();
    let code: Vec<_> = image.sections().into_iter().filter(|s| s.executable).collect();
    let base = image.image_base as usize;
    let pointer = |rva: usize| image.pointer_at(rva)?.checked_sub(base);
    rtti::image_vtables(image)
        .into_iter()
        .filter(|(_, name)| name == class)
        .map(|(vtable, _)| {
            let start = vtable - base;
            (0..MAX_VIRTUALS)
                .map_while(|slot| pointer(start + slot * pointer_size).filter(|rva| code.iter().any(|s| (s.start..s.end).contains(rva))))
                .collect()
        })
        .collect()
}

/// What each virtual function of the class's first vtable does with `this`: its accesses, by vtable slot. Calls are
/// followed, including the class's own virtual calls (through `this`'s vtable, which is the class's)
pub fn usage(image: &PeImage, class: &str) -> Vec<(usize, usize, Vec<Access>)> {
    let this = if image.is_64 { Register::RCX } else { Register::ECX };
    let pointer_size = image.pointer_size();
    let vtables = virtual_functions(image, class);
    let functions = vtables.first().cloned().unwrap_or_default();
    // `call [[this] + n]` is the vtable's slot n / pointer size
    let own_virtual = |chain: &[i64]| -> Option<usize> {
        match chain {
            [0, 0, offset] if *offset >= 0 && (*offset as usize).is_multiple_of(pointer_size) => {
                functions.get(*offset as usize / pointer_size).copied()
            }
            _ => None,
        }
    };
    let tracer = Tracer::new(image).with_limits(CALL_DEPTH, INSTRUCTIONS).with_resolver(&own_virtual);
    functions
        .iter()
        .enumerate()
        .map(|(slot, &rva)| (slot, rva, tracer.trace_object(rva, this)))
        .collect()
}

// The class of the object `drawable.model_instance` is in
const INSTANCE_DRAWABLE: &str = ".?AVInstanceDrawable@cdc@@";
// Virtual functions whose index can differ by up to this much between games (some have virtuals the others don't)
const SLOT_RANGE: usize = 8;
// A virtual function only counts as the same one in the other image if this many of its accesses line up
const MIN_ALIGNED: usize = 3;
// A field's accesses only show where it went if this many line up
const MIN_VOTES: f64 = 2.0;

/// Which RTTI class's objects each layout field is in, so where it is in another build or game can be worked out from
/// how the class's own code uses it (`derive_layout`). A "group.field" entry wins over its whole "group" (all of its
/// offsets, that is: not its read sizes or values, see `layout::FieldKind`). Groups whose structures
/// have no RTTI (instances, collision meshes, scripts, TR2013's aim sweep pools) aren't here.
pub const FIELD_CLASSES: &[(&str, &str)] = &[
    ("proxy", components::PROXY),
    ("transform", components::TRANSFORM),
    // Every component has its instance there, so any will do
    ("component", components::TRANSFORM),
    ("drawable.instance_drawable", components::DRAWABLE),
    ("drawable.model_instance", INSTANCE_DRAWABLE),
    ("collide.shapes", components::COLLIDE),
    ("collide.shape_count", components::COLLIDE),
    ("trigger_shape.shape", components::SHAPE),
    ("camera", rtti::GAME_RENDER),
    ("camera_controller", rtti::EXPLORATION_CAMERA),
    ("collision_scene", rtti::COLLISION_SCENE_CLIENT),
    ("collision_scene.in_client", rtti::MULTIBODY_CLIENT),
    ("ballistic", components::BALLISTIC),
    ("player", rtti::PLAYER),
    ("menu", rtti::MENU_STATE_MANAGER),
];

/// The class a layout field (`group.field`) is a field of, if it's known
pub fn class_of(name: &str) -> Option<&'static str> {
    let group = name.split_once('.').map_or(name, |(g, _)| g);
    let entry = |key: &str| FIELD_CLASSES.iter().find(|(k, _)| *k == key).map(|(_, class)| *class);
    entry(name).or_else(|| entry(group))
}

// The layout's fields that are offsets (`group.field`, offset), by the class they're in
fn fields_by_class(layout: &Layout) -> BTreeMap<&'static str, Vec<(String, usize)>> {
    let mut by_class: BTreeMap<&'static str, Vec<(String, usize)>> = BTreeMap::new();
    for field in layout.fields().into_iter().filter(|f| f.kind == FieldKind::Offset) {
        if let (Some(class), Some(offset)) = (class_of(&field.name), field.offset) {
            by_class.entry(class).or_default().push((field.name, offset));
        }
    }
    by_class
}

/// How sure `derive_layout` is of where a field went
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    // Nothing near it is used, or its neighbours moved by different amounts
    NotFound,
    // Moved as the nearest used field on one side of it did
    Guessed,
    // Between used fields that moved by the same amount
    Placed,
    // Used the same way by the class's code in both
    Matched,
}

/// Where a layout field is in another image, as `derive_layout` worked it out
#[derive(Debug, Clone)]
pub struct DerivedField {
    pub name: String,
    pub old: usize,
    // None if nothing says
    pub new: Option<usize>,
    pub confidence: Confidence,
    pub how: String,
}

// Where a field at `offset` went, from where the fields the code uses went (old offset -> new offset, by votes)
fn place(offset: usize, moved: &BTreeMap<i64, (i64, f64)>) -> (Option<usize>, Confidence, String) {
    let offset = offset as i64;
    match moved.get(&offset) {
        Some((new, votes)) if *votes >= MIN_VOTES => {
            return (Some(*new as usize), Confidence::Matched, format!("used the same way ({:.0}x)", votes))
        }
        Some((new, votes)) => {
            return (Some(*new as usize), Confidence::Guessed, format!("used the same way, but only {:.0}x", votes))
        }
        None => {}
    }
    let shift = |(old, (new, _)): (&i64, &(i64, f64))| (*old, new - old);
    let below = moved.range(..offset).next_back().map(shift);
    let above = moved.range(offset + 1..).next().map(shift);
    let moved_by = |shift: i64| (offset + shift >= 0).then(|| (offset + shift) as usize);
    match (below, above) {
        (Some((b, s)), Some((a, t))) if s == t => {
            (moved_by(s), Confidence::Placed, format!("between +{:#X} and +{:#X}, which both moved {:+#X}", b, a, s))
        }
        (Some((b, s)), Some((a, t))) => {
            (None, Confidence::NotFound, format!("+{:#X} moved {:+#X} but +{:#X} {:+#X}", b, s, a, t))
        }
        (Some((n, s)), None) | (None, Some((n, s))) => {
            (moved_by(s), Confidence::Guessed, format!("moved as +{:#X} did ({:+#X}), the only used field that side", n, s))
        }
        (None, None) => (None, Confidence::NotFound, "nothing near it is used".to_string()),
    }
}

// Old [0, offset, ...] chain -> new chain -> votes, from lining up the accesses of each of the class's virtual functions
// with those of the same function (the best lining up of those near its slot) in the other image
// `old_key` and `new_key` are lent, not given, to `align_by`, as they're used again for every slot
#[allow(clippy::needless_borrows_for_generic_args)]
fn class_votes(old: &PeImage, new: &PeImage, class: &str) -> (ChainVotes, usize) {
    let (old_usage, new_usage) = (usage(old, class), usage(new, class));
    // Pointers are 4 bytes in one game and 8 in another, so they're only matched as pointers, and another compiler picks
    // other instructions for the same thing, so only how deep, how big and whether it's followed are compared
    let key = |pointer_size: usize| {
        move |a: &Access| (a.chain.len(), if a.size == pointer_size { 0 } else { a.size }, a.loads_pointer)
    };
    let (old_key, new_key) = (key(old.pointer_size()), key(new.pointer_size()));
    let mut votes: ChainVotes = ChainVotes::new();
    let mut matched = 0;
    for (slot, _, accesses) in &old_usage {
        if accesses.is_empty() {
            continue;
        }
        let near = new_usage.iter().filter(|(s, _, _)| s.abs_diff(*slot) <= SLOT_RANGE);
        let best = near
            .map(|(s, _, other)| (crate::derive::align_by(accesses, other, &old_key, &new_key), s.abs_diff(*slot)))
            .max_by_key(|(aligned, distance)| (aligned.len(), std::cmp::Reverse(*distance)));
        if let Some((aligned, _)) = best.filter(|(aligned, _)| aligned.len() >= MIN_ALIGNED) {
            matched += 1;
            for (a, b) in aligned {
                *votes.entry(a.chain.clone()).or_default().entry(b.chain.clone()).or_default() += 1.0;
            }
        }
    }
    (votes, matched)
}

/// Where each field of `layout` (`old`'s) that's in a class with RTTI is in `new`, which can be another build of the same
/// game or another game of the engine (32 or 64-bit), from how each class's virtual functions use its fields: matched
/// accesses, or else moved by as much as the used fields either side of it (`place`)
pub fn derive_layout(old: &PeImage, layout: &Layout, new: &PeImage) -> Vec<DerivedField> {
    let mut derived = vec![];
    for (class, fields) in fields_by_class(layout) {
        let (votes, matched) = class_votes(old, new, class);
        // Where each field the code uses directly went: the most votes for it
        let mut pairs: BTreeMap<i64, BTreeMap<i64, f64>> = BTreeMap::new();
        for (old_chain, news) in &votes {
            for (new_chain, weight) in news {
                if old_chain.len() >= 2 && new_chain.len() == old_chain.len() {
                    *pairs.entry(old_chain[1]).or_default().entry(new_chain[1]).or_default() += weight;
                }
            }
        }
        let moved: BTreeMap<i64, (i64, f64)> = pairs
            .into_iter()
            .filter_map(|(old, news)| news.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|best| (old, best)))
            .collect();
        for (name, offset) in fields {
            let (new_offset, confidence, how) = if matched == 0 {
                (None, Confidence::NotFound, format!("no virtual function of {} lines up", class))
            } else {
                place(offset, &moved)
            };
            derived.push(DerivedField { name, old: offset, new: new_offset, confidence, how });
        }
    }
    derived
}

/// `debug class-layout <from> <to>`: the layout of `to` worked out from `from`'s (both an exe or dump of a known
/// version), against the layout the offsets file has for `to`
pub fn print_class_layout(from: &str, to: &str, from_version: Option<&str>, to_version: Option<&str>) -> Result<()> {
    let (old, new) = (crate::derive::load(from)?, crate::derive::load(to)?);
    let old_details = crate::derive::reference_details(&old, from_version)?;
    let new_details = crate::derive::reference_details(&new, to_version).ok();
    println!("From {} to {}", old_details, new_details.as_ref().map_or("an unknown build".to_string(), |d| d.to_string()));
    let mut derived = derive_layout(&old, &old_details.layout, &new);
    derived.sort_by(|a, b| b.confidence.cmp(&a.confidence).then(a.name.cmp(&b.name)));
    // Only what the offsets file says for `to` counts: a field it leaves at the default (ROTTR Steam's) isn't known,
    // unless `to` has no layout of its own
    let default = Layout::default();
    let known = |name: &str| {
        let layout = &new_details.as_ref()?.layout;
        let offset = field_offset(layout, name)?;
        (layout.is_default() || field_offset(&default, name) != Some(offset)).then_some(offset)
    };
    // (right, wrong, unchecked) by confidence
    let mut tally: BTreeMap<Confidence, (usize, usize, usize)> = BTreeMap::new();
    for field in &derived {
        let known = known(&field.name);
        let counts = tally.entry(field.confidence).or_default();
        let verdict = match (field.new, known) {
            (Some(new), Some(known)) if new == known => {
                counts.0 += 1;
                "right"
            }
            (Some(_), Some(_)) => {
                counts.1 += 1;
                "WRONG"
            }
            _ => {
                counts.2 += 1;
                ""
            }
        };
        println!(
            "  {:<9} {:<32} {:>7} -> {:>7}  {:<6} {}{}",
            format!("{:?}", field.confidence),
            field.name,
            format!("{:#X}", field.old),
            field.new.map_or("?".to_string(), |o| format!("{:#X}", o)),
            verdict,
            field.how,
            known.filter(|k| Some(*k) != field.new).map_or(String::new(), |k| format!(" (it's {:#X})", k)),
        );
    }
    for (confidence, (right, wrong, unchecked)) in tally.iter().rev() {
        println!("{:?}: {} right, {} wrong, {} not known in the offsets file", confidence, right, wrong, unchecked);
    }

    // What `update-offsets` would do with it, starting from `from`'s layout
    let same_game = new_details.as_ref().is_some_and(|d| d.name == old_details.name);
    let mut notes = vec![];
    let applied = crate::update_offsets::class_layout(
        old_details.layout.clone(),
        &[(&old, &old_details)],
        &new,
        same_game,
        &mut Default::default(),
        &mut notes,
    );
    println!("\nupdate-offsets would set ({}):", if same_game { "the same game" } else { "another game" });
    for note in &notes {
        let name = note.split_whitespace().next().unwrap_or("");
        let verdict = match (field_offset(&applied, name), known(name)) {
            (Some(set), Some(known)) if set == known => "right",
            (Some(_), Some(_)) => "WRONG",
            _ => "",
        };
        println!("  {:<6} {}", verdict, note);
    }
    Ok(())
}

/// `debug class-fields <image> <class> [--field <offset>]`
pub fn print_class_fields(image: &str, class: &str, field: Option<usize>) -> Result<()> {
    let image = crate::derive::load(image)?;
    let class = if class.starts_with(".?AV") { class.to_string() } else { format!(".?AV{}@@", class) };
    let field = field.map(|f| f as i64);
    let usage = usage(&image, &class);
    if usage.is_empty() {
        bail!("No vtable for {} in the image", class);
    }
    // Which slots use each field directly
    let mut by_field: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for (slot, rva, accesses) in &usage {
        let mut fields: Vec<String> = vec![];
        for access in accesses {
            if let [0, offset, rest @ ..] = access.chain.as_slice() {
                let what = format!(
                    "+{:#X}{} {:?}/{}",
                    offset,
                    rest.iter().map(|o| format!("->{:#X}", o)).collect::<String>(),
                    access.mnemonic,
                    access.size
                );
                if !fields.contains(&what) {
                    fields.push(what);
                }
                if rest.is_empty() {
                    by_field.entry(*offset).or_default().push(format!("{}@{:#X}", slot, rva));
                }
            }
        }
        if field.is_none() && !fields.is_empty() {
            println!("slot {:>3} +{:#X}: {}", slot, rva, fields.join(", "));
        }
    }
    match field {
        Some(f) => println!("+{:#X} is used by slots: {}", f, by_field.get(&f).map_or("none".to_string(), |s| s.join(", "))),
        None => {
            println!();
            for (offset, slots) in &by_field {
                println!("+{:#X}: {} slots", offset, slots.len());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_go_where_the_code_says_or_with_both_neighbours() {
        let moved: BTreeMap<i64, (i64, f64)> = vec![(0x10, (0x20, 3.0)), (0x40, (0x50, 2.0)), (0x80, (0xA0, 4.0)), (0x90, (0x90, 1.0))]
            .into_iter()
            .collect();
        assert_eq!(place(0x10, &moved).1, Confidence::Matched);
        assert_eq!(place(0x10, &moved).0, Some(0x20));
        // Between two that both moved +0x10
        assert_eq!(place(0x30, &moved), (Some(0x40), Confidence::Placed, place(0x30, &moved).2));
        // Between +0x10 and +0x20 moves
        assert_eq!(place(0x60, &moved).1, Confidence::NotFound);
        // One vote isn't enough
        assert_eq!(place(0x90, &moved).1, Confidence::Guessed);
    }

    #[test]
    fn fields_are_in_their_class_or_their_groups() {
        assert_eq!(class_of("proxy.centre"), Some(components::PROXY));
        assert_eq!(class_of("drawable.model_instance"), Some(INSTANCE_DRAWABLE));
        // Drawable's other fields are in objects with other classes, or none
        assert_eq!(class_of("drawable.mesh_min"), None);
        assert_eq!(class_of("collision_scene.in_client"), Some(rtti::MULTIBODY_CLIENT));
        assert_eq!(class_of("instance.position"), None);
    }
}
