//! What sets triggers off, and what they do. Triggers only notice something entering or leaving them; whether that's
//! Lara, a particular instance or anything at all is decided by the ActionGraph script nodes that use them, which refer
//! to each trigger by its instance's 64 bit id (`layout.instance.id`) rather than a pointer: an AGPlayerTriggerNode for
//! Lara, an AGInstanceTriggerNode for the instance whose id is also in the node. The nodes can't be reached from
//! anything the overlay reads, so they're found by scanning the game's heap for their vtables, in the background.
//!
//! The scripts have no names (everything in them is a hash), so what a trigger does comes from the nodes its node is
//! linked to in its graph (`layout.script`), by their class (e.g. a GameObjectiveNode is "objective"), and the instances
//! they act on or send messages to. Links into subgraphs aren't followed. A trigger's script only depends on its id, so
//! they're cached, and the heap is only scanned again for triggers that are new (or had no script, in case it's loaded
//! later).
//!
//! A game without those trigger nodes (TR2013) uses a trigger through an AGInstanceNode (or AGObjectNode) with its id,
//! like any other instance, whose outputs are its events. Which of them means Lara isn't known, so they're all "script".

use crate::game_memory::{pointer_from, pointer_size, read_pointer, read_u32, read_u64, u16_at};
use crate::layout::{self, ScriptLayout};
use crate::ui_ipc_data::trigger_action_bit;
use crate::pe_image::PeImage;
use process_memory::{CopyAddress, Pid, ProcessHandle};
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const PLAYER_NODE: &str = ".?AVAGPlayerTriggerNode@@";
const INSTANCE_NODE: &str = ".?AVAGInstanceTriggerNode@@";
const OTHER_NODES: [&str; 2] = [".?AVAGTriggerNode@@", ".?AVAGTriggerMasterNode@@"];
const OBJECT_NODE: &str = ".?AVAGObjectNode@@";
const MESSAGE_SEND_NODE: &str = ".?AVAGMessageSendNode@@";
// A node for an instance (its id at `layout.object_instance`, as in an AGObjectNode), which is how scripts use triggers
// in games without trigger nodes
const INSTANCE_REFERENCE_NODE: &str = ".?AVAGInstanceNode@@";
// What a script does, by part of the class names of the nodes a trigger leads to, most important first (the words are
// `TRIGGER_ACTIONS`)
const ACTIONS: [(&str, &str); 40] = [
    ("Checkpoint", "checkpoint"),
    ("GameObjective", "objective"),
    ("GlobalProgress", "progress"),
    ("Timeline", "cutscene"),
    ("MovesetProfile", "moveset"),
    ("Camera", "camera"),
    ("Streaming", "streaming"),
    ("StreamStates", "streaming"),
    ("UnitStream", "streaming"),
    ("UnloadUnit", "streaming"),
    ("Teleport", "teleport"),
    ("BattleConductor", "combat"),
    ("Spawn", "spawn"),
    ("GiveBirth", "spawn"),
    ("InstanceDestroy", "despawn"),
    ("FadeOut", "despawn"),
    ("HealthAndDamage", "damage"),
    ("Dialogue", "dialogue"),
    ("Subtitle", "dialogue"),
    ("SoundVocal", "dialogue"),
    ("Tutorial", "tutorial"),
    ("HeadTrack", "look at"),
    ("NPC", "NPC"),
    ("HuntManager", "NPC"),
    ("Inventory", "inventory"),
    ("Ammo", "inventory"),
    ("Campsite", "campsite"),
    ("Lighting", "lighting"),
    ("GraphicsOverride", "lighting"),
    ("Fog", "lighting"),
    ("Weather", "weather"),
    ("Wetness", "weather"),
    ("Wwise", "audio"),
    ("SoundPlex", "audio"),
    ("SoundReverb", "audio"),
    ("SFX", "audio"),
    ("Notification", "UI"),
    ("UI_", "UI"),
    ("PadShock", "rumble"),
    ("PersistentData", "saves state"),
];
const MAX_GRAPH_NODES: usize = 20000;
const MAX_LINKS: usize = 100000;
const CHUNK: usize = 64 << 20;
// How soon to scan again when there are triggers that haven't been looked for, or that had no script
const RESCAN_NEW: Duration = Duration::from_secs(10);
const RESCAN_NO_SCRIPT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Player,
    Instance,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetOffBy {
    Lara,
    // The instance named in the node, if it's loaded
    Instance(Option<String>),
    // Only by other kinds of script node
    Script,
    // Nothing uses it
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerScript {
    pub set_off_by: SetOffBy,
    /// What its scripts do (`ACTIONS`' words, most important first), then the names of the instances they act on
    pub does: Vec<String>,
    /// Which of `TRIGGER_ACTIONS` it counts as
    pub actions: u32,
}

// What a node the trigger leads to does
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    // An index in `ACTIONS`
    Action(usize),
    // Acts on the instance whose id is at this offset in the node
    ActsOn(usize),
}

// A trigger node, the 64 bit values in it, and what the nodes it leads to do (action indices and instance ids)
#[derive(Debug, Clone, Default)]
struct Node {
    kind: Option<NodeKind>,
    values: Vec<u64>,
    actions: Vec<usize>,
    targets: Vec<u64>,
}

#[derive(Debug, Default)]
struct Classes {
    triggers: HashMap<usize, NodeKind>,
    effects: HashMap<usize, Effect>,
    // Whether the game has no trigger nodes, so triggers are found through the nodes for any instance
    by_reference: bool,
}

// A graph's nodes and, for each, the nodes it links to
struct Graph {
    nodes: Vec<usize>,
    links: HashMap<u16, Vec<u16>>,
}

// The process handle can be used from any thread
struct SendHandle(ProcessHandle);
unsafe impl Send for SendHandle {}

pub struct TriggerScripts {
    pid: Pid,
    handle: ProcessHandle,
    base_address: usize,
    classes: Option<Arc<Classes>>,
    running: Option<JoinHandle<Result<(Arc<Classes>, Vec<Node>), String>>>,
    last_scan: Option<Instant>,
    known: HashMap<u64, TriggerScript>,
}

impl TriggerScripts {
    pub fn new(pid: Pid, handle: ProcessHandle, base_address: usize) -> TriggerScripts {
        TriggerScripts { pid, handle, base_address, classes: None, running: None, last_scan: None, known: HashMap::new() }
    }

    /// What sets the trigger with this id off and what it does, None if it hasn't been looked for yet
    pub fn script(&self, id: u64) -> Option<&TriggerScript> {
        self.known.get(&id)
    }

    /// Takes the results of a finished scan, and starts another if some of these triggers need it. `names` are the
    /// names of the instances by id.
    pub fn update(&mut self, triggers: &[u64], names: &HashMap<u64, String>) {
        if self.running.as_ref().map_or(false, |r| r.is_finished()) {
            match self.running.take().unwrap().join() {
                Ok(Ok((classes, nodes))) => {
                    self.classes = Some(classes);
                    let ids: HashSet<u64> = triggers.iter().copied().chain(self.known.keys().copied()).collect();
                    self.known.extend(categorise(&nodes, &ids, names));
                }
                Ok(Err(e)) => eprintln!("Couldn't find the trigger scripts: {}", e),
                Err(_) => eprintln!("Finding the trigger scripts failed"),
            }
        }
        if self.running.is_some() {
            return;
        }
        let since = self.last_scan.map(|t| t.elapsed());
        let unknown = triggers.iter().any(|id| !self.known.contains_key(id));
        let no_script = triggers.iter().any(|id| self.known.get(id).map(|s| &s.set_off_by) == Some(&SetOffBy::Nothing));
        let due = match since {
            None => !triggers.is_empty(),
            Some(since) => (unknown && since > RESCAN_NEW) || (no_script && since > RESCAN_NO_SCRIPT),
        };
        if due {
            self.last_scan = Some(Instant::now());
            let (pid, handle, base_address, classes) =
                (self.pid, SendHandle(self.handle), self.base_address, self.classes.clone());
            let layout = layout::current();
            self.running = Some(thread::spawn(move || {
                let handle = handle;
                scan(pid, handle.0, base_address, classes, &layout.script)
            }));
        }
    }
}

/// Finds what sets the triggers off and what they do now, rather than in the background (for `check`), and whether
/// they were found through the nodes for any instance, as the game has no trigger nodes
pub fn scripts_now(
    pid: Pid,
    handle: ProcessHandle,
    base_address: usize,
    triggers: &[u64],
    names: &HashMap<u64, String>,
) -> Result<(HashMap<u64, TriggerScript>, bool), String> {
    let (classes, nodes) = scan(pid, handle, base_address, None, &layout::current().script)?;
    Ok((categorise(&nodes, &triggers.iter().copied().collect(), names), classes.by_reference))
}

// What sets each trigger off, going by the nodes that have its id in them, and what those lead to
fn categorise(nodes: &[Node], triggers: &HashSet<u64>, names: &HashMap<u64, String>) -> HashMap<u64, TriggerScript> {
    let mut by_trigger: HashMap<u64, Vec<&Node>> = HashMap::new();
    for node in nodes {
        for value in &node.values {
            if triggers.contains(value) {
                by_trigger.entry(*value).or_default().push(node);
            }
        }
    }
    triggers
        .iter()
        .map(|id| {
            let using = by_trigger.get(id).map_or(&[][..], |n| &n[..]);
            let instance = using.iter().find(|n| n.kind == Some(NodeKind::Instance)).map(|node| {
                // The instance's id is in there too
                node.values.iter().filter(|v| !triggers.contains(v)).find_map(|v| names.get(v).cloned())
            });
            let set_off_by = match instance {
                Some(name) => SetOffBy::Instance(name),
                None if using.iter().any(|n| n.kind == Some(NodeKind::Player)) => SetOffBy::Lara,
                None if !using.is_empty() => SetOffBy::Script,
                None => SetOffBy::Nothing,
            };
            let mut actions: Vec<usize> = using.iter().flat_map(|n| n.actions.iter().copied()).collect();
            actions.sort();
            actions.dedup();
            let mut does: Vec<String> = actions.iter().map(|&a| ACTIONS[a].1.to_string()).collect();
            does.dedup();
            let mut bits = does.iter().fold(0, |bits, word| bits | trigger_action_bit(word));
            for target in using.iter().flat_map(|n| n.targets.iter()).filter(|t| *t != id) {
                if let Some(name) = names.get(target).map(|n| short_name(n)) {
                    bits |= trigger_action_bit("instances");
                    if !does.contains(&name) {
                        does.push(name);
                    }
                }
            }
            if set_off_by == SetOffBy::Nothing {
                bits |= trigger_action_bit("no script");
            } else if does.is_empty() {
                bits |= trigger_action_bit("something else");
            }
            (*id, TriggerScript { set_off_by, does, actions: bits })
        })
        .collect()
}

// Without the prefixes most of the instances scripts act on have (`tr11_animhost_friendly_npc_roaming_musician_guitar`)
fn short_name(name: &str) -> String {
    let mut name = name;
    for prefix in ["tr11_", "animhost_", "friendly_"] {
        name = name.strip_prefix(prefix).unwrap_or(name);
    }
    name.to_string()
}

// Every trigger script node in the game's heap, and what each leads to
fn scan(
    pid: Pid,
    handle: ProcessHandle,
    base_address: usize,
    classes: Option<Arc<Classes>>,
    layout: &ScriptLayout,
) -> Result<(Arc<Classes>, Vec<Node>), String> {
    let classes = match classes {
        Some(c) => c,
        None => {
            let image = PeImage::dump(pid).map_err(|e| format!("couldn't read the game: {}", e))?;
            Arc::new(find_classes(&image.data, base_address, layout))
        }
    };
    if classes.triggers.is_empty() {
        return Err("no trigger node classes in the game".to_string());
    }

    let image_end = base_address + crate::find_process::get_image_size(handle, base_address).unwrap_or(0);
    let lowest = *classes.triggers.keys().min().unwrap();
    let highest = *classes.triggers.keys().max().unwrap();
    let pointer_size = pointer_size();
    let mut nodes = vec![];
    let mut chunk = vec![0u8; CHUNK];
    for (start, size) in heap_regions(handle) {
        if start < image_end && start + size > base_address {
            continue;
        }
        for offset in (0..size).step_by(CHUNK) {
            let length = CHUNK.min(size - offset);
            if handle.copy_address(start + offset, &mut chunk[..length]).is_err() {
                continue;
            }
            for (i, word) in chunk[..length].chunks_exact(pointer_size).enumerate() {
                let value = pointer_from(word);
                // Vtables are in the game's image, which rules out nearly everything without a lookup
                if value < lowest || value > highest {
                    continue;
                }
                if let Some(kind) = classes.triggers.get(&value) {
                    let address = start + offset + i * pointer_size;
                    let mut node = vec![0u8; layout.node_size];
                    if handle.copy_address(address, &mut node).is_ok() {
                        let values = node.chunks_exact(8).map(|b| u64::from_le_bytes(b.try_into().unwrap())).collect();
                        nodes.push((address, Node { kind: Some(*kind), values, ..Default::default() }));
                    }
                }
            }
        }
    }

    let mut graphs: HashMap<usize, Option<Graph>> = HashMap::new();
    let nodes = nodes
        .into_iter()
        .map(|(address, mut node)| {
            let graph = read_pointer(handle, address + layout.node_graph).unwrap_or(0);
            let graph = graphs.entry(graph).or_insert_with(|| read_graph(handle, layout, graph));
            // Nodes with more than one vtable are found once per vtable, but only the start of one is in its graph
            if let Some(graph) = graph {
                if let Some(index) = graph.nodes.iter().position(|&n| n == address) {
                    for reached in leads_to(graph, index) {
                        let vtable = read_pointer(handle, reached).unwrap_or(0);
                        match classes.effects.get(&vtable) {
                            Some(Effect::Action(action)) => node.actions.push(*action),
                            Some(Effect::ActsOn(offset)) => node.targets.push(read_u64(handle, reached + offset).unwrap_or(0)),
                            None => {}
                        }
                    }
                }
            }
            node
        })
        .collect();
    Ok((classes, nodes))
}



fn read_graph(handle: ProcessHandle, layout: &ScriptLayout, graph: usize) -> Option<Graph> {
    let nodes_at = read_pointer(handle, graph.checked_add(layout.graph_nodes)?)?;
    let header = read_pointer(handle, graph.checked_add(layout.graph_links)?)?;
    let node_count = read_u32(handle, header.checked_add(layout.node_count)?)? as usize;
    let link_count = read_u32(handle, header.checked_add(layout.link_count)?)? as usize;
    if node_count > MAX_GRAPH_NODES || link_count > MAX_LINKS {
        return None;
    }
    let pointer_size = pointer_size();
    let mut node_bytes = vec![0u8; node_count * pointer_size];
    handle.copy_address(nodes_at, &mut node_bytes).ok()?;
    let mut link_bytes = vec![0u8; link_count * layout.link_size];
    let links_at = read_pointer(handle, header.checked_add(layout.links)?)?;
    handle.copy_address(links_at, &mut link_bytes).ok()?;
    let nodes = node_bytes.chunks_exact(pointer_size).map(pointer_from).collect();
    let mut links: HashMap<u16, Vec<u16>> = HashMap::new();
    for link in link_bytes.chunks_exact(layout.link_size) {
        let (from, to) = (u16_at(link, 0), u16_at(link, 2));
        links.entry(from).or_default().push(to);
    }
    Some(Graph { nodes, links })
}

// The nodes this one leads to, directly or through others
fn leads_to(graph: &Graph, start: usize) -> Vec<usize> {
    let mut seen = HashSet::new();
    seen.insert(start as u16);
    let mut next = vec![start as u16];
    let mut reached = vec![];
    while let Some(from) = next.pop() {
        for &to in graph.links.get(&from).map_or(&[][..], |l| &l[..]) {
            if seen.insert(to) {
                if let Some(&node) = graph.nodes.get(to as usize).filter(|&&n| n != 0) {
                    reached.push(node);
                }
                next.push(to);
            }
        }
    }
    reached
}

// Trigger nodes' vtables, and those of nodes that say what a script does
fn find_classes(data: &[u8], base_address: usize, layout: &ScriptLayout) -> Classes {
    let vtables = crate::rtti::all_vtables(data, base_address);
    let trigger_kind = |name: &str| match name {
        PLAYER_NODE => Some(NodeKind::Player),
        INSTANCE_NODE => Some(NodeKind::Instance),
        n if OTHER_NODES.contains(&n) => Some(NodeKind::Other),
        _ => None,
    };
    let by_reference = !vtables.iter().any(|(_, name)| trigger_kind(name).is_some());
    let mut classes = Classes { by_reference, ..Default::default() };
    for (vtable, name) in vtables {
        let kind = match name.as_str() {
            INSTANCE_REFERENCE_NODE | OBJECT_NODE if by_reference => Some(NodeKind::Other),
            n => trigger_kind(n),
        };
        if let Some(kind) = kind {
            classes.triggers.insert(vtable, kind);
            // They're also what scripts act on
            if by_reference {
                classes.effects.insert(vtable, Effect::ActsOn(layout.object_instance));
            }
            continue;
        }
        let effect = match name.as_str() {
            OBJECT_NODE => Some(Effect::ActsOn(layout.object_instance)),
            MESSAGE_SEND_NODE => layout.message_instance.map(Effect::ActsOn),
            n => ACTIONS.iter().position(|(part, _)| n.contains(part)).map(Effect::Action),
        };
        if let Some(effect) = effect {
            classes.effects.insert(vtable, effect);
        }
    }
    classes
}

// Committed, readable, private memory (the heaps)
#[cfg(windows)]
fn heap_regions(handle: ProcessHandle) -> Vec<(usize, usize)> {
    use winapi::um::memoryapi::VirtualQueryEx;
    use winapi::um::winnt::{
        MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_PRIVATE, PAGE_EXECUTE_READWRITE, PAGE_READONLY,
        PAGE_READWRITE,
    };
    let mut regions = vec![];
    let mut address = 0usize;
    loop {
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let written =
            unsafe { VirtualQueryEx(handle, address as *const _, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        if written == 0 {
            break;
        }
        // Plain pages only: write combined or uncached ones (graphics memory) are very slow to read, and don't have
        // script nodes in them
        let readable = [PAGE_READWRITE, PAGE_READONLY, PAGE_EXECUTE_READWRITE].contains(&info.Protect);
        if info.State == MEM_COMMIT && info.Type == MEM_PRIVATE && readable {
            regions.push((info.BaseAddress as usize, info.RegionSize));
        }
        match (info.BaseAddress as usize).checked_add(info.RegionSize) {
            Some(next) if next > address => address = next,
            _ => break,
        }
    }
    regions
}

#[cfg(not(windows))]
fn heap_regions(_handle: ProcessHandle) -> Vec<(usize, usize)> {
    panic!("tomb-helper is only supported on Windows");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: NodeKind, values: Vec<u64>) -> Node {
        Node { kind: Some(kind), values, ..Default::default() }
    }

    #[test]
    fn triggers_are_categorised_by_the_nodes_using_them() {
        let (trigger, lara_trigger, instance_trigger, unused, musician) = (1u64, 2, 3, 4, 100);
        let nodes = vec![
            node(NodeKind::Other, vec![0, trigger]),
            node(NodeKind::Player, vec![lara_trigger, 7]),
            node(NodeKind::Player, vec![instance_trigger]),
            node(NodeKind::Instance, vec![instance_trigger, musician]),
        ];
        let triggers: HashSet<u64> = vec![trigger, lara_trigger, instance_trigger, unused].into_iter().collect();
        let names: HashMap<u64, String> = vec![(musician, "musician".to_string())].into_iter().collect();
        let found = categorise(&nodes, &triggers, &names);
        assert_eq!(found[&trigger].set_off_by, SetOffBy::Script);
        assert_eq!(found[&lara_trigger].set_off_by, SetOffBy::Lara);
        assert_eq!(found[&instance_trigger].set_off_by, SetOffBy::Instance(Some("musician".to_string())));
        assert_eq!(found[&unused].set_off_by, SetOffBy::Nothing);
    }

    #[test]
    fn what_triggers_do_is_most_important_first_then_who_they_act_on() {
        let (trigger, other_trigger, musician) = (1u64, 2, 100);
        let objective = ACTIONS.iter().position(|a| a.1 == "objective").unwrap();
        let audio = ACTIONS.iter().position(|a| a.1 == "audio").unwrap();
        let mut using = node(NodeKind::Player, vec![trigger]);
        using.actions = vec![audio, objective, audio];
        // Itself, a trigger that isn't loaded and an instance that is
        using.targets = vec![trigger, other_trigger, musician];
        let triggers: HashSet<u64> = vec![trigger].into_iter().collect();
        let names: HashMap<u64, String> =
            vec![(musician, "tr11_animhost_friendly_npc_musician".to_string())].into_iter().collect();
        let found = categorise(&[using], &triggers, &names);
        assert_eq!(found[&trigger].does, vec!["objective", "audio", "npc_musician"]);
        let bits = ["objective", "audio", "instances"].iter().fold(0, |b, w| b | trigger_action_bit(w));
        assert_eq!(found[&trigger].actions, bits);
    }

    #[test]
    fn every_action_can_be_shown_by() {
        for (_, word) in ACTIONS {
            assert_ne!(trigger_action_bit(word), 0, "{}", word);
        }
    }

    #[test]
    fn links_are_followed_through_other_nodes() {
        // 0 -> 2 -> 3, 1 -> 4, and 3 -> 0 back round; slot 5 is empty
        let graph = Graph {
            nodes: vec![10, 11, 12, 13, 14, 0],
            links: vec![(0, vec![2]), (2, vec![3, 5]), (1, vec![4]), (3, vec![0])].into_iter().collect(),
        };
        let mut reached = leads_to(&graph, 0);
        reached.sort();
        assert_eq!(reached, vec![12, 13]);
    }
}
