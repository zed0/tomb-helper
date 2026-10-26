//! The values tomb-helper writes to the game: Lara's position (and ROTTR's look at), what flight mode holds or turns off
//! (`Velocity`, `FallStart`, her proxy's, `CameraCollision`) and the cutscene's status. For each, where it is
//! (`candidates`, `find`) and whether it looks right (`verify`), which the handlers check before writing anything, so a
//! chain that's wrong for a build is never written through. `check` reports the same.
//!
//! Where the game's structures say where something is, the chain is built from them (`structural`), which carries over
//! to a new build whose structures haven't changed, and is preferred to the offsets file's. Lara's position chain
//! (`XPosition`) is [.., player, `player.instance`, `instance.position`], so the same chain less its last offset leads to
//! her instance, and through it her components (by the slots learnt for their classes, see `components`), and one more
//! offset less to her player object. The camera's collision is a path (`camera.collision`) from GameRender, which
//! `CameraView`'s chain starts at. Each is checked by the RTTI class of the object it's in, where that's known.

use crate::components::{self, Components};
use crate::game::Game;
use crate::game_memory::{pointer_size, read_floats, read_u32, resolve};
use crate::layout;
use crate::process_details::{AddressOffsets, AddressType};
use nalgebra::Vector3;
use process_memory::CopyAddress;

// The player object Lara's position chain goes through
pub const PLAYER: &str = ".?AVPlayer@@";
// The camera controller whose collision `camera.collision` leads to; the others (aiming, stealth, ...) are laid out
// differently, so it's only written while this one is in use
const EXPLORATION_CAMERA: &str = ".?AVExplorationCameraController@@";

// Things of Lara's should be this close to her
const NEAR_LARA: f32 = 300.0;
// Her physics body can lag behind her proxy (it stays put while its collision is off), but not by this much
const NEAR_BODY: f32 = 5000.0;
// Faster than this (per second) is garbage, as is a fall start this far above or below her
const MAX_SPEED: f32 = 100_000.0;
const MAX_FALL: f32 = 100_000.0;
// Coordinates beyond this are garbage
const MAX_COORDINATE: f32 = 1e7;
// The cutscene's prompt and status are small numbers, and its length (in seconds) at most this
const MAX_CUTSCENE_STATE: u8 = 16;
const MAX_CUTSCENE_LENGTH: f32 = 36_000.0;

/// Everything tomb-helper writes, in the order `check` reports them
pub const WRITTEN: [AddressType; 18] = [
    AddressType::XPosition,
    AddressType::YPosition,
    AddressType::ZPosition,
    AddressType::XLookAt,
    AddressType::YLookAt,
    AddressType::ZLookAt,
    AddressType::Velocity,
    AddressType::FallStart,
    AddressType::FallStartCopy,
    AddressType::ProxyCentre,
    AddressType::ProxyResolvedCentre,
    AddressType::ProxyFlags,
    AddressType::ProxyShapeCentre,
    AddressType::ProxyShapePreviousCentre,
    AddressType::ProxyShapeGap,
    AddressType::ProxyBodyGap,
    AddressType::CameraCollision,
    AddressType::CutsceneStatus,
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source {
    // Built from the layout and what the game's other chains go through
    Structure,
    OffsetsFile,
}

impl Source {
    pub fn describe(self) -> &'static str {
        match self {
            Source::Structure => "the structures",
            Source::OffsetsFile => "the offsets file",
        }
    }
}

/// Whether a value looks like what it should be, with what was found
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Plausible(String),
    Implausible(String),
    // Its chain doesn't resolve, or it can't be read (e.g. there's no cutscene), which says nothing either way
    Unreadable(String),
}

/// Where the value can be: through the structures first, then the offsets file's chain (if they're different)
pub fn candidates(game: &Game, components: &mut Components, address_type: AddressType) -> Vec<(Vec<usize>, Source)> {
    let mut found = vec![];
    if let Some(chain) = structural(game, components, address_type) {
        found.push((chain, Source::Structure));
    }
    if let Some(chain) = game.offsets.get(&address_type) {
        if found.iter().all(|(c, _)| c != chain) {
            found.push((chain.clone(), Source::OffsetsFile));
        }
    }
    found
}

/// The first of the candidates that's plausible, or why there isn't one
pub fn find(game: &Game, components: &mut Components, address_type: AddressType) -> Result<(Vec<usize>, Source), String> {
    let mut reasons = vec![];
    for (chain, source) in candidates(game, components, address_type) {
        match verify(game, components, address_type, &chain) {
            Verdict::Plausible(_) => return Ok((chain, source)),
            Verdict::Implausible(why) | Verdict::Unreadable(why) => {
                reasons.push(format!("through {} {}: {}", source.describe(), describe_chain(&chain), why))
            }
        }
    }
    if reasons.is_empty() {
        Err("this version has no way to find it".to_string())
    } else {
        Err(reasons.join("; "))
    }
}

/// The chain to the value from the game's structures, if they say where it is
pub fn structural(game: &Game, components: &mut Components, address_type: AddressType) -> Option<Vec<usize>> {
    let slot = |class: &str| -> Option<usize> {
        let prefix = lara_instance(&game.offsets)?;
        let instance = resolve(game.handle, game.base_address, &[prefix, vec![0]].concat())?;
        components.get(game.handle, instance, class)?;
        components.known_slots().get(class).copied()
    };
    chain_for(&game.offsets, address_type, slot)
}

// `structural`, with the slot of each of Lara's components learnt by `slot`
fn chain_for(offsets: &AddressOffsets, address_type: AddressType, mut slot: impl FnMut(&str) -> Option<usize>) -> Option<Vec<usize>> {
    let layout = layout::current();
    let then = |prefix: Vec<usize>, rest: &[Option<usize>]| -> Option<Vec<usize>> {
        let rest: Option<Vec<usize>> = rest.iter().copied().collect();
        Some(prefix.into_iter().chain(rest?).collect())
    };
    // The chain to the pointer to one of her components, in her component table
    let mut component = |class: &str| -> Option<Vec<usize>> {
        let table = layout.instance.components?;
        Some([lara_instance(offsets)?, vec![table, slot(class)? * pointer_size()]].concat())
    };
    let (proxy_layout, body, shape) = (&layout.proxy, &layout.body, &layout.physics_shape);
    if let AddressType::Velocity = address_type {
        // Her cdc::BallisticComponent: in her component table, or where the layout says her instance points at it
        let ballistic = component(components::BALLISTIC)
            .or_else(|| Some([lara_instance(offsets)?, vec![layout.instance.ballistic?]].concat()))?;
        return then(ballistic, &[layout.ballistic.velocity]);
    }
    let mut proxy = || component(components::PROXY);
    match address_type {
        AddressType::FallStart => then(player(offsets)?, &[layout.player.fall_start]),
        AddressType::FallStartCopy => then(player(offsets)?, &[layout.player.fall_start_copy]),
        AddressType::ProxyCentre => then(proxy()?, &[Some(proxy_layout.centre)]),
        AddressType::ProxyResolvedCentre => then(proxy()?, &[proxy_layout.resolved_centre]),
        // Only where which bit is its collision is known, as that's what flight mode changes
        AddressType::ProxyFlags if proxy_layout.movement_collides.is_some() => then(proxy()?, &[proxy_layout.flags]),
        AddressType::ProxyShapeCentre => then(proxy()?, &[proxy_layout.body, Some(body.shape), Some(shape.centre)]),
        AddressType::ProxyShapePreviousCentre => {
            then(proxy()?, &[proxy_layout.body, Some(body.shape), shape.previous_centre])
        }
        AddressType::ProxyShapeGap => then(proxy()?, &[proxy_layout.body, Some(body.shape), shape.gap]),
        AddressType::ProxyBodyGap => then(proxy()?, &[proxy_layout.body, body.gap]),
        AddressType::CameraCollision if !layout.camera.collision.is_empty() => {
            Some(game_render(offsets)?.into_iter().chain(layout.camera.collision.iter().copied()).collect())
        }
        _ => None,
    }
}

// The chain to the pointer to Lara's instance: her position chain less its last offset, if it ends in her instance's
// position
fn lara_instance(offsets: &AddressOffsets) -> Option<Vec<usize>> {
    let chain = offsets.get(&AddressType::XPosition)?;
    match chain.split_last() {
        Some((&last, prefix)) if !prefix.is_empty() && last == layout::current().instance.position => Some(prefix.to_vec()),
        _ => None,
    }
}

/// The layout with what Lara's position chain says of it: `XPosition` is [.., player, `player.instance`,
/// `instance.position`], so where the player points at her is the chain's second last offset (where the version has a
/// player, and the chain ends in her instance's position). For a new build, whose chain was derived, with what changed
pub fn layout_from_chains(mut layout: crate::layout::Layout, offsets: &AddressOffsets) -> (crate::layout::Layout, Option<String>) {
    let chain = match offsets.get(&AddressType::XPosition) {
        Some(chain) if chain.len() >= 3 => chain,
        _ => return (layout, None),
    };
    let (n, current) = (chain.len(), layout.player.instance);
    if chain[n - 1] != layout.instance.position || current.is_none() || current == Some(chain[n - 2]) {
        return (layout, None);
    }
    layout.player.instance = Some(chain[n - 2]);
    let note = format!("{:<34} {:#X} -> {:#X} (XPosition's chain)", "player.instance", current.unwrap_or(0), chain[n - 2]);
    (layout, Some(note))
}

// The chain to the pointer to the player object: her position chain less two offsets, if the one before her
// instance's position is where the player points at her
// (or, where the layout has a path from her instance to it, her instance's chain and then that)
fn player(offsets: &AddressOffsets) -> Option<Vec<usize>> {
    let layout = layout::current();
    if !layout.player.from_instance.is_empty() {
        return Some([lara_instance(offsets)?, layout.player.from_instance.clone()].concat());
    }
    let chain = offsets.get(&AddressType::XPosition)?;
    let n = chain.len();
    (n >= 3 && Some(chain[n - 2]) == layout.player.instance).then(|| chain[..n - 2].to_vec())
}

// The chain to the pointer to GameRender: `CameraView`'s less the view's offset
fn game_render(offsets: &AddressOffsets) -> Option<Vec<usize>> {
    match offsets.get(&AddressType::CameraView)?.as_slice() {
        [render, view] if *view == layout::current().camera.view => Some(vec![*render]),
        _ => None,
    }
}

/// Whether the value at the end of `chain` looks like the value it's meant to be
pub fn verify(game: &Game, components: &mut Components, address_type: AddressType, chain: &[usize]) -> Verdict {
    let address = match resolve(game.handle, game.base_address, chain) {
        Some(address) => address,
        None => return Verdict::Unreadable("its chain doesn't resolve".to_string()),
    };
    let floats = |count: usize| -> Option<Vec<f32>> {
        let all: [f32; 4] = match count {
            1 => read_floats::<1>(game.handle, address).map(|[a]| [a, 0.0, 0.0, 0.0])?,
            3 => read_floats::<3>(game.handle, address).map(|[a, b, c]| [a, b, c, 0.0])?,
            _ => read_floats::<4>(game.handle, address)?,
        };
        Some(all[..count].to_vec())
    };
    let unreadable = || Verdict::Unreadable("it can't be read".to_string());
    let lara = game.lara_position().map(|p| p.vector());
    let verdict = |ok: bool, what: String| if ok { Verdict::Plausible(what) } else { Verdict::Implausible(what) };
    let near = |values: &[f32], within: f32| -> Verdict {
        let point = Vector3::new(values[0], values[1], values[2]);
        match &lara {
            Ok(lara) => {
                let distance = (point - lara).norm();
                verdict(distance < within, format!("{:.0} from Lara", distance))
            }
            Err(_) => Verdict::Unreadable("Lara's position can't be read".to_string()),
        }
    };
    let value = match address_type {
        AddressType::XPosition
        | AddressType::YPosition
        | AddressType::ZPosition
        | AddressType::XLookAt
        | AddressType::YLookAt
        | AddressType::ZLookAt => match floats(1) {
            Some(v) => verdict(v[0].is_finite() && v[0].abs() < MAX_COORDINATE, format!("{:.1}", v[0])),
            None => unreadable(),
        },
        AddressType::Velocity => match floats(3) {
            Some(v) => {
                let speed = Vector3::new(v[0], v[1], v[2]).norm();
                verdict(speed.is_finite() && speed < MAX_SPEED, format!("speed {:.1}", speed))
            }
            None => unreadable(),
        },
        AddressType::FallStart | AddressType::FallStartCopy => match (floats(1), &lara) {
            (Some(v), Ok(lara)) => {
                let below = lara.z - v[0];
                verdict(below.is_finite() && below.abs() < MAX_FALL, format!("{:.0} from Lara's height", below))
            }
            (None, _) => unreadable(),
            (_, Err(_)) => Verdict::Unreadable("Lara's position can't be read".to_string()),
        },
        AddressType::ProxyCentre | AddressType::ProxyResolvedCentre => match floats(3) {
            Some(v) => near(&v, NEAR_LARA),
            None => unreadable(),
        },
        AddressType::ProxyShapeCentre | AddressType::ProxyShapePreviousCentre => match floats(3) {
            Some(v) => near(&v, NEAR_BODY),
            None => unreadable(),
        },
        AddressType::ProxyShapeGap | AddressType::ProxyBodyGap => match floats(4) {
            Some(v) => {
                let size = Vector3::new(v[0], v[1], v[2]).norm();
                verdict(v.iter().all(|f| f.is_finite()) && size < NEAR_BODY, format!("{:.1} to go", size))
            }
            None => unreadable(),
        },
        AddressType::ProxyFlags => match read_u32(game.handle, address) {
            Some(flags) => match layout::current().proxy.movement_collides {
                Some(collides) => verdict(flags & collides as u32 != 0, format!("flags {:#X}", flags)),
                None => Verdict::Implausible(format!("flags {:#X}, but which bit is its collision isn't known", flags)),
            },
            None => unreadable(),
        },
        AddressType::CameraCollision => match read_byte(game, address) {
            Some(enabled) => verdict(enabled <= 1, format!("enabled {}", enabled)),
            None => unreadable(),
        },
        AddressType::CutsceneStatus => cutscene(game),
        _ => Verdict::Unreadable(format!("{:?} isn't something tomb-helper writes", address_type)),
    };
    // What it's in should be what it's meant to be
    match (value, owner_check(game, components, address_type, chain)) {
        (Verdict::Plausible(what), Some(Err(why))) => Verdict::Implausible(format!("{}, but {}", what, why)),
        (Verdict::Plausible(what), Some(Ok(owner))) => Verdict::Plausible(format!("{}, in {}", what, owner)),
        (value, _) => value,
    }
}

// The class the object the value is in should have (or for her position, that it's in her instance), if it's known:
// Ok with what it is, or why not
fn owner_check(game: &Game, components: &mut Components, address_type: AddressType, chain: &[usize]) -> Option<Result<String, String>> {
    let (_, prefix) = chain.split_last()?;
    if prefix.is_empty() {
        return None;
    }
    let owner = resolve(game.handle, game.base_address, &[prefix.to_vec(), vec![0]].concat());
    let class = match address_type {
        AddressType::Velocity => components::BALLISTIC,
        AddressType::FallStart | AddressType::FallStartCopy => PLAYER,
        AddressType::ProxyCentre | AddressType::ProxyResolvedCentre | AddressType::ProxyFlags => components::PROXY,
        AddressType::CameraCollision => EXPLORATION_CAMERA,
        // Only where the version's instances and their components are known
        AddressType::XPosition | AddressType::YPosition | AddressType::ZPosition
            if game.has(AddressType::InstanceArray) && layout::current().instance.components.is_some() =>
        {
            let is_lara = owner.map_or(false, |instance| components.get(game.handle, instance, components::PLAYER).is_some());
            return Some(if is_lara {
                Ok("an instance with a PlayerComponent".to_string())
            } else {
                Err("it isn't in an instance with a PlayerComponent".to_string())
            });
        }
        _ => return None,
    };
    let found = owner.and_then(|owner| components.classes.name(game.handle, owner));
    Some(match found {
        Some(name) if name == class => Ok(a(&short_class(class))),
        Some(name) => Err(format!("it's in {}, not {}", a(&short_class(&name)), a(&short_class(class)))),
        None => Err(format!("it isn't in {}", a(&short_class(class)))),
    })
}

// "a Player", "an ExplorationCameraController"
fn a(noun: &str) -> String {
    let vowel = noun.chars().next().map_or(false, |c| "AEIOUaeiou".contains(c));
    format!("{} {}", if vowel { "an" } else { "a" }, noun)
}

/// A chain as the offsets file has it, e.g. [0x14B5BD8, 0x0, 0x33E8, 0x10]
pub fn describe_chain(chain: &[usize]) -> String {
    format!("[{}]", chain.iter().map(|o| format!("{:#X}", o)).collect::<Vec<_>>().join(", "))
}

// ".?AVBallisticComponent@cdc@@" as "cdc::BallisticComponent"
fn short_class(class: &str) -> String {
    let parts: Vec<&str> = class.trim_start_matches(".?AV").trim_end_matches("@@").split('@').collect();
    parts.iter().rev().copied().collect::<Vec<_>>().join("::")
}

fn read_byte(game: &Game, address: usize) -> Option<u8> {
    let mut byte = [0u8; 1];
    game.handle.copy_address(address, &mut byte).ok()?;
    Some(byte[0])
}

// The cutscene's state, which only reads while there's one: its prompt and status small numbers, its timeline within
// its length
fn cutscene(game: &Game) -> Verdict {
    let byte = |address_type| game.resolve(address_type).and_then(|address| read_byte(game, address));
    let float = |address_type| game.resolve(address_type).and_then(|address| read_floats::<1>(game.handle, address)).map(|[f]| f);
    let (status, prompt) = match (byte(AddressType::CutsceneStatus), byte(AddressType::CutscenePrompt)) {
        (Some(status), Some(prompt)) => (status, prompt),
        _ => return Verdict::Unreadable("there's no cutscene".to_string()),
    };
    let mut what = format!("status {}, prompt {}", status, prompt);
    let mut sane = status < MAX_CUTSCENE_STATE && prompt < MAX_CUTSCENE_STATE;
    if let (Some(timeline), Some(length)) = (float(AddressType::CutsceneTimeline), float(AddressType::CutsceneLength)) {
        sane &= length.is_finite() && (0.0..MAX_CUTSCENE_LENGTH).contains(&length) && timeline.is_finite() && timeline >= 0.0;
        what = format!("{}, {:.1}s of {:.1}s", what, timeline, length);
    }
    // What the blacklist and timing info go by: a cutscene always has one (a chain into the wrong field of the right
    // object read 0 in SOTTR 234.1)
    if game.has(AddressType::CutsceneId) {
        match game.resolve(AddressType::CutsceneId).and_then(|address| read_u32(game.handle, address)) {
            Some(id) => {
                sane &= id != 0;
                what = format!("{}, id {:#X}", what, id);
            }
            None => {
                sane = false;
                what = format!("{}, but its id can't be read", what);
            }
        }
    }
    if sane {
        Verdict::Plausible(what)
    } else {
        Verdict::Implausible(what)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SOTTR Steam 492's layout rebuilds the chains found for it by hand (which were in the offsets file until the
    // structures were known to give the same), with her proxy in slot 39 as it is there
    #[test]
    fn structures_give_the_chains_found_by_hand() {
        let details = crate::offsets_file::known_process_details()
            .into_iter()
            .find(|d| d.version.version == "(Steam) v1.0 build 492.0_64")
            .unwrap();
        crate::layout::set_for(&details);
        let lara = [0x14B5BD8, 0x0, 0x33E8];
        let proxy = [0x14B5BD8, 0x0, 0x33E8, 0xD8, 0x138];
        let body = [0x14B5BD8, 0x0, 0x33E8, 0xD8, 0x138, 0x1A8];
        let found_by_hand: Vec<(AddressType, Vec<usize>)> = vec![
            (AddressType::Velocity, [&lara[..], &[0xF0, 0x20]].concat()),
            (AddressType::FallStart, vec![0x14B5BD8, 0x0, 0x1064]),
            (AddressType::FallStartCopy, vec![0x14B5BD8, 0x0, 0x1070]),
            (AddressType::ProxyCentre, [&proxy[..], &[0x70]].concat()),
            (AddressType::ProxyResolvedCentre, [&proxy[..], &[0xC0]].concat()),
            (AddressType::ProxyFlags, [&proxy[..], &[0x18]].concat()),
            (AddressType::ProxyShapeCentre, [&body[..], &[0x20, 0x0]].concat()),
            (AddressType::ProxyShapePreviousCentre, [&body[..], &[0x20, 0xA0]].concat()),
            (AddressType::ProxyShapeGap, [&body[..], &[0x20, 0x10]].concat()),
            (AddressType::ProxyBodyGap, [&body[..], &[0x0]].concat()),
            (AddressType::CameraCollision, vec![0x36ABA00, 0x450, 0xF08, 0x20, 0xDF8]),
        ];
        let slot = |class: &str| (class == components::PROXY).then(|| 39);
        for (address_type, chain) in found_by_hand {
            assert_eq!(chain_for(&details.address_offsets, address_type, slot), Some(chain), "{:?}", address_type);
        }
    }

    #[test]
    fn classes_are_shown_as_in_code() {
        assert_eq!(short_class(components::BALLISTIC), "cdc::BallisticComponent");
        assert_eq!(short_class(PLAYER), "Player");
    }
}
