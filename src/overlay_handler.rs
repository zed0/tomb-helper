//! The overlay: on `ToggleUI` injects `tomb_helper_ui.dll` into the game, or `tomb_helper_ui32.dll` into a 32-bit one
//! (`inject32`), and each tick sends it what to draw through shared memory (`ui_ipc_data`): Lara's position, the camera
//! (with a projection made from `CameraLens`), boxes round objects (rescanned every second), triggers, the level's
//! collision near her, and her own collision (with a 150ms hold on "pushing", as contacts are often caught mid update).
//! Without the camera or instance array the overlay only shows its window. It sets `should_exit` to make the overlay
//! eject itself when toggled off or dropped. `debug overlay-data` prints what it would send without injecting.

use anyhow::{anyhow, Context, Result};
use crate::game::Game;
use crate::game_memory::Handle;
use crate::collision::{self, AttachedProxy, CapsuleShape, ProxyCapsules};
use crate::collision_mesh::CollisionMeshes;
use crate::components::Components;
use crate::config::{Hotkey, OverlayConfig};
use crate::object_list::{self, Instance, ObjectList};
use crate::position::Position;
use crate::action::Action;
use crate::handler::Handler;
use crate::inject32;
use crate::process_details::{AddressOffsets, AddressType};
use nalgebra::{Matrix3, Vector3};
use std::collections::HashMap;
use std::{env, io, time};
use std::error::Error;
use hudhook::inject::Process;
use process_memory::{Architecture, Pid, ProcessHandle};
use crate::trigger::{self, Shape};
use crate::trigger_scripts::{SetOffBy, TriggerScript, TriggerScripts};
use crate::handler::HelperState;
use crate::ui_ipc_data::{REQUEST_RESTORE_POSITION, REQUEST_STORE_POSITION, REQUEST_TOGGLE_FLYING, name_bytes, text_bytes, trigger_action_bit, trigger_action_mask, projection_from_lens, BoxKind, Capsule, HotkeyLabel, Line, ObjectBox, ObjectCategory, SharedMemoryManager, TriggerCategory, TriggerFace, TriggerLabel, UiSettings, HOTKEY_LABELS, HOTKEY_LABEL_LENGTH, MAX_ATTACHED, MAX_OBJECTS, MAX_TRIANGLES, MAX_TRIGGERS};

// How far the camera's ray is followed to find where Lara's aiming
const MAX_AIM_DISTANCE: f32 = 20000.0;
// How far above her feet the middle of Lara is (her standing capsule's centre), which counts as her being in a trigger
// as well as her feet
const LARA_MIDDLE: f32 = 85.0;
// How often to rescan the instance array for objects matching the filters
const OBJECT_REFRESH_INTERVAL: time::Duration = time::Duration::from_secs(1);
// Lara's contacts are often caught mid update, so keep showing an attached proxy as touching for this long
const TOUCHING_HOLD: time::Duration = time::Duration::from_millis(150);

// Which objects the window says to show
#[derive(Clone, Copy, Default, PartialEq)]
struct ObjectSettings {
    interactables: bool,
    triggers: bool,
    trigger_categories: u32,
    npcs: bool,
    // Show triggers by `TRIGGER_ACTIONS` rather than by category, and which
    triggers_by_action: bool,
    trigger_actions: u32,
}

// Which collision meshes the window says to draw
#[derive(Clone, Copy, Default, PartialEq)]
struct MeshSettings {
    // Those Lara collides with, and the rest
    coarse: bool,
    detailed: bool,
    // The kinds of collision that are detailed
    other_kinds: u32,
}

// An object to box
struct ShownObject {
    instance: Instance,
    kind: BoxKind,
    category: ObjectCategory,
}

// A trigger to draw
struct ShownTrigger {
    instance: Instance,
    shape: Shape,
    category: TriggerCategory,
    label: String,
    // The `TRIGGER_ACTIONS` index it's coloured by
    action: u32,
}

impl MeshSettings {
    fn any(&self) -> bool {
        self.coarse || self.detailed
    }
}

// What the window says to draw this tick
struct TickSettings {
    objects: ObjectSettings,
    meshes: MeshSettings,
    // Lara's capsules, probes and proxies
    lara_collision: bool,
    // The meshes round where Lara's aiming, while she aims
    follow_aim: bool,
    // How far from Lara (or where she's aiming) the meshes are drawn
    coarse_radius: f32,
    detailed_radius: f32,
}

impl TickSettings {
    // Hiding the overlay turns everything off at once, and "Collision meshes" all the collision
    fn read(settings: &UiSettings) -> TickSettings {
        let shown = settings.overlay.get();
        let collision = shown && settings.collision.get();
        let meshes = MeshSettings {
            coarse: collision && settings.coarse_meshes.get(),
            detailed: collision && settings.detailed_meshes.get(),
            other_kinds: settings.other_kinds.get(),
        };
        TickSettings {
            objects: ObjectSettings {
                interactables: shown && settings.interactables.get(),
                triggers: shown && settings.triggers.get(),
                trigger_categories: settings.trigger_categories.get(),
                npcs: shown && settings.npcs.get(),
                triggers_by_action: settings.triggers_by_action.get(),
                trigger_actions: settings.trigger_actions.get(),
            },
            follow_aim: meshes.any() && settings.follow_aim.get(),
            meshes,
            lara_collision: collision && settings.lara_collision.get(),
            coarse_radius: settings.coarse_radius.get(),
            detailed_radius: settings.detailed_radius.get(),
        }
    }
}

// Lara's proxies as drawn, touching if they were within `TOUCHING_HOLD`
struct DrawnProxies {
    shapes: Vec<Capsule>,
    touching: bool,
    attached: Vec<Capsule>,
    attached_touching: [bool; MAX_ATTACHED],
    attached_active: bool,
}

// The shown triggers' world space outlines, faces and labels
#[derive(Default)]
struct TriggerGeometry {
    labels: Vec<TriggerLabel>,
    lines: Vec<Line>,
    faces: Vec<TriggerFace>,
}

pub struct OverlayHandler {
    active: bool,
    // For reading the game's structures, and where things are in it
    game: Game,
    // For injecting the overlay
    pid: Pid,
    arch: Architecture,
    config: OverlayConfig,
    // As of this tick
    position: Position,
    // The camera, for drawing in the world, if this game has the offsets, else only the window is shown. As of this
    // tick; zeros, or None, without them.
    camera_view: [f32; 16],
    camera_lens: Option<[f32; 8]>,
    collision_meshes: CollisionMeshes,
    // What the other handlers have to show, and actions the window's buttons asked for
    helper_state: HelperState,
    requested: Vec<Action>,
    // As of the last refresh
    mesh_settings: MeshSettings,
    object_list: Option<ObjectList>,
    objects: Vec<ShownObject>,
    // Characters' and animals' hit capsules in bone space, by instance. Their models' bounds are often only part of
    // them (people are several models), so they're boxed round these instead.
    npc_capsules: HashMap<usize, Vec<CapsuleShape>>,
    triggers: Vec<ShownTrigger>,
    trigger_scripts: TriggerScripts,
    components: Components,
    // Found by her PlayerComponent each refresh
    lara_address: Option<usize>,
    // As of the last refresh
    object_settings: ObjectSettings,
    proxy_last_touching: Option<time::Instant>,
    attached_last_touching: [Option<time::Instant>; MAX_ATTACHED],
    // Lara's instance and her capsules in bone space
    lara: Option<(usize, Vec<CapsuleShape>)>,
    last_object_refresh: Option<time::Instant>,
    ui_memory_manager: SharedMemoryManager,
    // The keys bound to the window's controls, for it to show
    hotkeys: [[u8; HOTKEY_LABEL_LENGTH]; HOTKEY_LABELS],
}

impl OverlayHandler {
    pub fn new_overlay_handler(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
        pid: &Pid,
        config: &OverlayConfig,
        hotkeys: &[Hotkey],
    ) -> Option<OverlayHandler> {
        println!("Loading overlay handler...");
        let game = Game::new(Handle::Live(*pid, *handle), *base_addr, address_offsets);
        if ![AddressType::XPosition, AddressType::YPosition, AddressType::ZPosition].iter().all(|&a| game.has(a)) {
            return None;
        }

        let mut ui_memory_manager = SharedMemoryManager::new(true);
        // Kept when the handler is recreated (e.g. when the offsets change)
        let settings = &ui_memory_manager.get().settings;
        if !settings.initialised.get() {
            initialise_settings(settings, config);
        }
        ui_memory_manager.get_mut().update(|val| {
            val.debug = config.console;
            val.should_exit = false;
            val.object_count = 0;
            val.triangle_count = 0;
        });

        Some(OverlayHandler {
            active: false,
            game,
            pid: *pid,
            arch: *arch,
            config: config.clone(),
            position: Position::default(),
            camera_view: [0.0; 16],
            camera_lens: None,
            collision_meshes: CollisionMeshes::default(),
            mesh_settings: MeshSettings::default(),
            helper_state: HelperState::default(),
            requested: vec![],
            object_list: None,
            objects: vec![],
            npc_capsules: HashMap::new(),
            triggers: vec![],
            trigger_scripts: TriggerScripts::new(Handle::Live(*pid, *handle), *base_addr),
            components: Components::new(*base_addr),
            lara_address: None,
            object_settings: ObjectSettings::default(),
            lara: None,
            last_object_refresh: None,
            proxy_last_touching: None,
            attached_last_touching: [None; MAX_ATTACHED],
            ui_memory_manager,
            hotkeys: hotkey_labels(hotkeys),
        })
    }

    // Finds Lara, and the objects and triggers near her to show
    fn refresh_objects(&mut self, settings: ObjectSettings) -> Result<(), Box<dyn Error>> {
        if self.object_list.is_none() {
            if !self.game.has(AddressType::InstanceArray) {
                return Ok(());
            }
            self.object_list = Some(ObjectList::new(self.game.require(AddressType::InstanceArray)?));
        }

        let lara = self.position;
        let all = self.object_list.as_mut().unwrap().instances(self.game.handle)?;
        let names: HashMap<u64, String> = all.iter().map(|i| (i.id, i.name.clone())).collect();
        let mut nearby: Vec<Instance> =
            all.into_iter().filter(|i| lara.distance(&i.position) <= self.config.object_radius).collect();
        nearby.sort_by(|a, b| lara.distance(&a.position).total_cmp(&lara.distance(&b.position)));

        self.components.start_refresh();
        let nearest: Vec<usize> = nearby.iter().map(|i| i.address).collect();
        self.lara_address = self.game.find_lara(&mut self.components, &nearest);

        let (objects, triggers) = self.choose(nearby, &settings);
        // What sets them off is found in the background, so it can change from "unchecked" between refreshes
        if settings.triggers {
            let ids: Vec<u64> = triggers.iter().map(|(i, _)| i.id).collect();
            self.trigger_scripts.update(&ids, &names);
        }
        self.triggers = self.shown_triggers(triggers, &settings);
        self.objects = self.shown_objects(objects)?;
        Ok(())
    }

    // Which of the instances near Lara to box, and the triggers that might be shown (by what they do, once that's known)
    fn choose(
        &mut self,
        nearby: Vec<Instance>,
        settings: &ObjectSettings,
    ) -> (Vec<ShownObject>, Vec<(Instance, TriggerCategory)>) {
        let filters: Vec<String> = self.config.object_filters.iter().map(|f| f.to_lowercase()).collect();
        let mut objects = vec![];
        let mut triggers = vec![];
        for instance in nearby {
            let name = instance.name.to_lowercase();
            let full_name = instance.full_name.to_lowercase();
            let named = filters.iter().any(|f| name.contains(f.as_str()) || full_name.contains(f.as_str()));
            let kinds = if settings.interactables || settings.triggers || settings.npcs {
                trigger::read_kinds(self.game.handle, &mut self.components, instance.address)
            } else {
                trigger::Kinds::default()
            };
            let category = trigger::categorise(&instance.name, &instance.full_name);
            let wanted = settings.triggers_by_action || settings.trigger_categories & category.bit() != 0;
            if settings.triggers && kinds.trigger && wanted {
                triggers.push((instance.clone(), category));
            }
            // She has her own collision drawn, and counts as interactable herself (she has an InteractableComponent)
            let is_lara = Some(instance.address) == self.lara_address;
            let category = if named {
                ObjectCategory::Named
            } else if is_lara {
                continue;
            } else if kinds.npc {
                // They have interaction components too (for looting them)
                if !settings.npcs {
                    continue;
                }
                ObjectCategory::Npc
            } else if settings.interactables && kinds.interactable && trigger::for_lara(&instance.name) {
                ObjectCategory::Interactable
            } else {
                continue;
            };
            objects.push(ShownObject { instance, kind: BoxKind::Model, category });
        }
        (objects, triggers)
    }

    // The triggers to show, with their shapes and labels
    fn shown_triggers(&mut self, candidates: Vec<(Instance, TriggerCategory)>, settings: &ObjectSettings) -> Vec<ShownTrigger> {
        let mut shown = vec![];
        for (instance, category) in candidates {
            let script = self.trigger_scripts.script(instance.id);
            let does = script.map_or(trigger_action_bit("unchecked"), |s| s.actions);
            if (settings.triggers_by_action && settings.trigger_actions & does == 0) || shown.len() >= MAX_TRIGGERS {
                continue;
            }
            if let Some(shape) = trigger::read_shape(self.game.handle, &mut self.components, instance.address) {
                let label = trigger_label(&instance.name, script);
                // The first of what it does that's shown (all of it when shown by name)
                let shown_does = if settings.triggers_by_action { does & settings.trigger_actions } else { does };
                shown.push(ShownTrigger { instance, shape, category, label, action: shown_does.trailing_zeros() });
            }
        }
        shown
    }

    // The objects to box, and Lara if her hit capsules are to be drawn but she has none, with their models' bounds
    fn shown_objects(&mut self, objects: Vec<ShownObject>) -> Result<Vec<ShownObject>, Box<dyn Error>> {
        let mut boxes = vec![];
        self.lara = None;
        if let (true, Some(address)) = (self.config.lara_capsules, self.lara_address) {
            let shapes = collision::read_capsule_shapes(self.game.handle, &mut self.components, address)?;
            if shapes.is_empty() {
                // Better than nothing
                boxes.push(ShownObject {
                    instance: Instance {
                        address,
                        id: 0,
                        position: object_list::read_position(self.game.handle, address)?,
                        name: "Lara".to_string(),
                        full_name: String::new(),
                        bounds: None,
                    },
                    kind: BoxKind::Lara,
                    category: ObjectCategory::Named,
                });
            } else {
                self.lara = Some((address, shapes));
            }
        }
        boxes.extend(objects);
        boxes.truncate(MAX_OBJECTS);

        // Models can change (e.g. Lara's outfit), so these are refreshed with the list
        self.npc_capsules.clear();
        for ShownObject { instance: object, kind, category } in &mut boxes {
            object.bounds = object_list::read_bounds(self.game.handle, &mut self.components, object.address);
            if object.bounds.is_none() && *kind == BoxKind::Model {
                *kind = BoxKind::Estimated;
            }
            if *category == ObjectCategory::Npc {
                if let Ok(shapes) = collision::read_capsule_shapes(self.game.handle, &mut self.components, object.address) {
                    if !shapes.is_empty() {
                        self.npc_capsules.insert(object.address, shapes);
                    }
                }
            }
        }
        Ok(boxes)
    }

    // Actions the window's buttons asked for, which `take_actions` hands on
    fn take_requests(&mut self) {
        let requests = self.ui_memory_manager.get().settings.requests.swap(0, std::sync::atomic::Ordering::SeqCst);
        for (bit, action) in [
            (REQUEST_STORE_POSITION, Action::StorePosition {}),
            (REQUEST_RESTORE_POSITION, Action::RestorePosition {}),
            (REQUEST_TOGGLE_FLYING, Action::ToggleActive {}),
        ] {
            if requests & bit != 0 {
                self.requested.push(action);
            }
        }
    }

    // The objects every so often, and the objects and meshes straight away when their settings change
    fn refresh(&mut self, settings: &TickSettings) -> Result<(), Box<dyn Error>> {
        let refresh_due = self.last_object_refresh.map_or(true, |t| t.elapsed() > OBJECT_REFRESH_INTERVAL);
        if refresh_due || settings.objects != self.object_settings {
            self.last_object_refresh = Some(time::Instant::now());
            self.object_settings = settings.objects;
            self.refresh_objects(settings.objects)?;
        }
        if refresh_due || settings.meshes != self.mesh_settings {
            self.mesh_settings = settings.meshes;
            if settings.meshes.any() && self.game.has(AddressType::CollisionScene) {
                let MeshSettings { coarse, detailed, other_kinds } = settings.meshes;
                let scene = self.game.require(AddressType::CollisionScene)?;
                self.collision_meshes.refresh(self.game.handle, scene, coarse, detailed, other_kinds)?;
            }
        }
        Ok(())
    }

    // Lara's proxies to draw (none unless `show`), and whether she's aiming: her attached proxies being used is the best
    // sign of it. She can be reloaded (and moved) between refreshes, which `components.get` notices.
    fn lara_proxies(&mut self, show: bool, follow_aim: bool) -> (DrawnProxies, bool) {
        let proxy = if show || follow_aim {
            self.game.lara_proxy(&mut self.components, self.lara_address)
        } else {
            None
        };
        let proxies = proxy.and_then(|proxy| self.game.lara_proxies(proxy).ok());
        let aiming = proxies.as_ref().map_or(false, |p| p.attached_active);
        let ProxyCapsules { shapes, touching, attached, attached_active } = match proxies {
            Some(proxies) if show => proxies,
            _ => ProxyCapsules::default(),
        };
        let (touching, attached_touching) = self.hold_touching(touching, &attached);
        let attached = attached.iter().map(|proxy| proxy.capsule).collect();
        (DrawnProxies { shapes, touching, attached, attached_touching, attached_active }, aiming)
    }

    // Whether Lara's proxy and each attached one were touching something in the last `TOUCHING_HOLD`
    fn hold_touching(&mut self, touching: bool, attached: &[AttachedProxy]) -> (bool, [bool; MAX_ATTACHED]) {
        let held = |last: Option<time::Instant>| last.map_or(false, |t| t.elapsed() < TOUCHING_HOLD);
        if touching {
            self.proxy_last_touching = Some(time::Instant::now());
        }
        let mut attached_touching = [false; MAX_ATTACHED];
        for (i, proxy) in attached.iter().take(MAX_ATTACHED).enumerate() {
            if proxy.touching {
                self.attached_last_touching[i] = Some(time::Instant::now());
            }
            attached_touching[i] = held(self.attached_last_touching[i]);
        }
        (held(self.proxy_last_touching), attached_touching)
    }

    // The objects' boxes where they are now
    fn object_boxes(&self) -> Vec<ObjectBox> {
        let size = self.config.object_box_size;
        let mut boxes = vec![];
        for ShownObject { instance: object, kind, category } in &self.objects {
            // Objects can be unloaded between refreshes
            let transform = match object_list::read_transform(self.game.handle, object.address) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let (min, max) = object.bounds.unwrap_or((
                Position { x: -size, y: -size, z: -size },
                Position { x: size, y: size, z: size },
            ));
            // Round the capsules where they are now
            let placed = self
                .npc_capsules
                .get(&object.address)
                .and_then(|shapes| collision::place_capsules(self.game.handle, object.address, shapes).ok())
                .and_then(|capsules| capsule_bounds(&capsules, &transform));
            let (min, max) = placed.unwrap_or((min, max));
            boxes.push(ObjectBox { transform, min, max, kind: *kind, category: *category, name: name_bytes(&object.name) });
        }
        boxes
    }

    // The triggers' outlines, faces and labels where they are now
    fn trigger_geometry(&self) -> TriggerGeometry {
        let lara = self.position;
        let mut geometry = TriggerGeometry::default();
        for ShownTrigger { instance: trigger, shape, category, label, action } in &self.triggers {
            let transform = match object_list::read_transform(self.game.handle, trigger.address) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let (category, action) = (*category, *action);
            let outline = trigger::outline(shape, &transform);
            let origin = Position { x: transform[12], y: transform[13], z: transform[14] };
            geometry.labels.push(TriggerLabel {
                position: top_of(&outline).unwrap_or(origin),
                category,
                action,
                name: text_bytes(label),
            });
            let inside = [0.0, LARA_MIDDLE]
                .iter()
                .any(|up| trigger::contains(shape, &transform, &Position { z: lara.z + up, ..lara }));
            geometry.lines.extend(outline.into_iter().map(|(a, b)| Line { a, b, category, action, inside }));
            let faces = trigger::faces(shape, &transform);
            geometry.faces.extend(faces.into_iter().map(|corners| TriggerFace { corners, category, action }));
        }
        geometry
    }

    // Where the camera's looking (the reticle's in the middle of the screen): the first collision triangle along it
    fn aim_point(&mut self) -> Option<Position> {
        // Camera to world, which starts at `CameraSin`
        let m: [f32; 16] = self.game.read_floats(AddressType::CameraSin).ok()?;
        // Its last row is where it is, the one before which way it faces
        let origin = Vector3::new(m[12], m[13], m[14]);
        let forward = Vector3::new(m[8], m[9], m[10]).try_normalize(1e-6)?;
        let hit = self.collision_meshes.raycast(self.game.handle, origin, forward, MAX_AIM_DISTANCE)?;
        Some(Position::from(origin + forward * hit))
    }
}

// A trigger's name and what sets it off, and on a second line what it does, once that's known
fn trigger_label(name: &str, script: Option<&TriggerScript>) -> String {
    let script = match script {
        Some(script) => script,
        None => return name.to_string(),
    };
    let by = match &script.set_off_by {
        SetOffBy::Lara => "Lara",
        SetOffBy::Instance(Some(name)) => name.as_str(),
        SetOffBy::Instance(None) => "an instance",
        SetOffBy::Script => "script",
        SetOffBy::Nothing => "no script",
    };
    let label = format!("{} ({})", name, by);
    if script.does.is_empty() {
        label
    } else {
        format!("{}\n{}", label, script.does.join(", "))
    }
}

// The keys bound to each of the window's controls, e.g. "F5", or "F5, NumPad5"
fn hotkey_labels(hotkeys: &[Hotkey]) -> [[u8; HOTKEY_LABEL_LENGTH]; HOTKEY_LABELS] {
    let mut keys: Vec<Vec<String>> = vec![vec![]; HOTKEY_LABELS];
    for hotkey in hotkeys {
        let control = match hotkey.action {
            Action::ToggleActive {} => HotkeyLabel::Flight,
            Action::StorePosition {} => HotkeyLabel::SavePosition,
            Action::RestorePosition {} => HotkeyLabel::RestorePosition,
            Action::ToggleSettingsWindow {} => HotkeyLabel::Window,
            Action::ToggleOverlay {} => HotkeyLabel::Overlay,
            Action::ToggleCollision {} => HotkeyLabel::Collision,
            Action::ToggleLaraCollision {} => HotkeyLabel::LaraCollision,
            Action::ToggleCoarseCollision {} => HotkeyLabel::CoarseMeshes,
            Action::ToggleDetailedCollision {} => HotkeyLabel::DetailedMeshes,
            Action::ToggleInteractables {} => HotkeyLabel::Interactables,
            Action::ToggleTriggers {} => HotkeyLabel::Triggers,
            Action::ToggleNpcs {} => HotkeyLabel::Npcs,
            _ => continue,
        };
        keys[control as usize].push(format!("{:?}", hotkey.key));
    }
    let mut labels = [[0; HOTKEY_LABEL_LENGTH]; HOTKEY_LABELS];
    for (label, keys) in labels.iter_mut().zip(keys) {
        *label = text_bytes(&keys.join(", "));
    }
    labels
}

// The window's settings start as the config says
fn initialise_settings(settings: &UiSettings, config: &OverlayConfig) {
    settings.overlay.set(true);
    settings.collision.set(config.collision);
    settings.lara_collision.set(true);
    settings.coarse_meshes.set(config.collision_meshes);
    settings.detailed_meshes.set(config.detailed_collision_meshes);
    settings.other_kinds.set(config.collision_mesh_other_kinds);
    settings.coarse_radius.set(config.collision_mesh_radius);
    settings.detailed_radius.set(config.detailed_collision_mesh_radius);
    settings.mesh_opacity.set(config.collision_opacity);
    settings.trigger_opacity.set(config.trigger_opacity);
    settings.fade_with_distance.set(config.collision_mesh_fade);
    settings.fade_strength.set(config.collision_mesh_fade_strength);
    settings.cull_hidden.set(config.collision_mesh_cull_hidden);
    settings.follow_aim.set(config.collision_mesh_follow_aim);
    settings.interactables.set(config.interactables);
    settings.triggers.set(config.triggers);
    settings.npcs.set(config.npcs);
    settings.trigger_categories.set(TriggerCategory::mask(&config.trigger_categories));
    settings.triggers_by_action.set(config.triggers_by_action);
    settings.trigger_actions.set(trigger_action_mask(&config.trigger_actions));
    settings.show_window.set(config.settings_window);
    settings.initialised.set(true);
}

// The middle of the top of an outline (world z is up), for its label
fn top_of(outline: &[(Position, Position)]) -> Option<Position> {
    let points = || outline.iter().flat_map(|(a, b)| [a, b]);
    let fold = |f: fn(f32, f32) -> f32, axis: fn(&Position) -> f32, start: f32| points().map(axis).fold(start, f);
    if outline.is_empty() {
        return None;
    }
    let middle = |axis: fn(&Position) -> f32| (fold(f32::min, axis, f32::MAX) + fold(f32::max, axis, f32::MIN)) / 2.0;
    Some(Position { x: middle(|p| p.x), y: middle(|p| p.y), z: fold(f32::max, |p| p.z, f32::MIN) })
}

// The box round the (world space) capsules in the space of the object-to-world `transform` (row-major, row
// vectors), so it turns with the object. None if there aren't any capsules, or the transform can't be inverted.
fn capsule_bounds(capsules: &[Capsule], transform: &[f32; 16]) -> Option<(Position, Position)> {
    // Columns are the object's axes in world space
    let axes = Matrix3::from_column_slice(&[
        transform[0], transform[1], transform[2],
        transform[4], transform[5], transform[6],
        transform[8], transform[9], transform[10],
    ]);
    let to_object = axes.try_inverse()?;
    let origin = Vector3::new(transform[12], transform[13], transform[14]);
    // A world space radius along each object axis
    let scale = Vector3::new(axes.column(0).norm(), axes.column(1).norm(), axes.column(2).norm());

    let mut bounds: Option<(Vector3<f32>, Vector3<f32>)> = None;
    for capsule in capsules {
        let radius = Vector3::repeat(capsule.radius).component_div(&scale);
        for end in [capsule.a, capsule.b] {
            let point = to_object * (end.vector() - origin);
            let (low, high) = (point - radius, point + radius);
            bounds = Some(match bounds {
                Some((min, max)) => (min.inf(&low), max.sup(&high)),
                None => (low, high),
            });
        }
    }
    let (min, max) = bounds?;
    Some((Position::from(min), Position::from(max)))
}

// Copies as many items as fit, returns how many that was
fn fill<T: Copy>(slots: &mut [T], items: &[T]) -> u32 {
    let count = items.len().min(slots.len());
    slots[..count].copy_from_slice(&items[..count]);
    count as u32
}

impl Handler for OverlayHandler {
    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        if !self.active {
            return Ok(());
        }

        self.position = self.game.lara_position()?;
        if self.game.has(AddressType::CameraView) {
            self.camera_view = self.game.read_floats(AddressType::CameraView)?;
        }
        if self.game.has(AddressType::CameraLens) {
            self.camera_lens = Some(self.game.read_floats(AddressType::CameraLens)?);
        }
        self.take_requests();
        let settings = TickSettings::read(&self.ui_memory_manager.get().settings);
        self.refresh(&settings)?;

        let position = self.position;
        let probes = match (settings.lara_collision && self.config.lara_probes, self.game.probes()) {
            (true, Some(probes)) => collision::probes_near(&probes?, &position),
            _ => vec![],
        };
        let (proxies, aiming) = self.lara_proxies(settings.lara_collision && self.config.lara_proxy, settings.follow_aim);
        let objects = self.object_boxes();
        let triggers = self.trigger_geometry();
        let capsules = match (settings.lara_collision, &self.lara) {
            (true, Some((address, shapes))) => collision::place_capsules(self.game.handle, *address, shapes)?,
            _ => vec![],
        };

        let started = time::Instant::now();
        let aim_point = if settings.follow_aim && aiming { self.aim_point() } else { None };
        let mesh_centre = aim_point.unwrap_or(position);
        let triangles = if settings.meshes.any() {
            let (coarse, detailed) = (settings.coarse_radius, settings.detailed_radius);
            self.collision_meshes.triangles_near(self.game.handle, &mesh_centre, coarse, detailed, MAX_TRIANGLES)
        } else {
            vec![]
        };
        let triangle_milliseconds = started.elapsed().as_secs_f32() * 1000.0;

        let camera_view = self.camera_view;
        let camera = &crate::layout::current().camera;
        let lens_fields = [camera.lens_near, camera.lens_far, camera.lens_fov, camera.lens_aspect].map(|f| f as u32);
        let camera_projection = self.camera_lens.map_or([0.0; 16], |lens| projection_from_lens(&lens, lens_fields));
        // 0 tells the overlay there's no camera
        let address = |address_type: AddressType| -> io::Result<u64> {
            if self.game.has(address_type) {
                self.game.require(address_type).map(|a| a as u64)
            } else {
                Ok(0)
            }
        };
        let camera_view_address = address(AddressType::CameraView)?;
        let camera_lens_address = address(AddressType::CameraLens)?;
        let kind_counts = self.collision_meshes.kind_counts;
        let helper_state = self.helper_state.clone();
        let hotkeys = self.hotkeys;
        self.ui_memory_manager.get_mut().update(|val| {
            val.position_tools = helper_state.position_tools;
            val.has_saved_position = helper_state.saved_position.is_some();
            val.saved_position = helper_state.saved_position.unwrap_or_default();
            val.flying = helper_state.flying;
            val.hotkeys = hotkeys;
            val.mesh_centre = mesh_centre;
            val.aiming = aim_point.is_some();
            val.position = position;
            val.camera_view = camera_view;
            val.camera_projection = camera_projection;
            val.lens_fields = lens_fields;
            val.camera_view_address = camera_view_address;
            val.camera_lens_address = camera_lens_address;
            val.object_count = fill(&mut val.objects, &objects);
            val.probe_count = fill(&mut val.probes, &probes);
            val.proxy_count = fill(&mut val.proxies, &proxies.shapes);
            val.proxy_touching = proxies.touching;
            val.attached_count = fill(&mut val.attached, &proxies.attached);
            val.attached_touching = proxies.attached_touching;
            val.attached_active = proxies.attached_active;
            val.capsule_count = fill(&mut val.capsules, &capsules);
            val.trigger_count = fill(&mut val.triggers, &triggers.labels);
            val.trigger_line_count = fill(&mut val.trigger_lines, &triggers.lines);
            val.trigger_face_count = fill(&mut val.trigger_faces, &triggers.faces);
            val.triangle_count = fill(&mut val.triangles, &triangles);
            val.triangle_milliseconds = triangle_milliseconds;
            val.mesh_kind_counts = kind_counts;
        });
        Ok(())
    }
    fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.requested)
    }

    fn show(&mut self, state: &HelperState) {
        self.helper_state = state.clone();
    }

    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>> {
        match action {
            Action::ToggleUI {} => {
                self.active = !self.active;

                if self.active {
                    self.ui_memory_manager.get_mut().update(|val| val.should_exit = false);

                    println!("Injecting overlay dll");
                    // A 32-bit game (TR2013) needs the 32-bit build of the overlay, injected its own way
                    let injected = if matches!(self.arch, Architecture::Arch32Bit) {
                        let dll_path = env::current_exe()?.with_file_name("tomb_helper_ui32.dll");
                        inject32::inject(self.pid, &dll_path)
                    } else {
                        let dll_path = env::current_exe()?.with_file_name("tomb_helper_ui.dll");
                        Process::by_pid(&self.pid)
                            .and_then(|process| process.inject(dll_path))
                            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
                    };
                    if let Err(e) = injected {
                        self.active = false;
                        return Err(format!("Could not inject the overlay: {}", e).into());
                    }
                } else {
                    println!("Ejecting injected overlay dll");
                    self.ui_memory_manager.get_mut().update(|val| val.should_exit = true);
                    println!("done");
                }
            },
            Action::ToggleLaraCollision {} => self.ui_memory_manager.get().settings.lara_collision.toggle(),
            Action::ToggleCoarseCollision {} => self.ui_memory_manager.get().settings.coarse_meshes.toggle(),
            Action::ToggleDetailedCollision {} => self.ui_memory_manager.get().settings.detailed_meshes.toggle(),
            Action::ToggleInteractables {} => self.ui_memory_manager.get().settings.interactables.toggle(),
            Action::ToggleTriggers {} => self.ui_memory_manager.get().settings.triggers.toggle(),
            Action::ToggleNpcs {} => self.ui_memory_manager.get().settings.npcs.toggle(),
            Action::ToggleCollision {} => self.ui_memory_manager.get().settings.collision.toggle(),
            Action::ToggleOverlay {} => self.ui_memory_manager.get().settings.overlay.toggle(),
            Action::ToggleSettingsWindow {} => self.ui_memory_manager.get().settings.show_window.toggle(),
            _ => (),
        }
        Ok(())
    }
}

/// `debug overlay-data`: what the overlay would be sent now, from one update with collision on, without injecting it
pub fn print_overlay_data(version: Option<String>, delay: u64) -> Result<()> {
    if delay > 0 {
        println!("Starting in {} seconds...", delay);
        std::thread::sleep(time::Duration::from_secs(delay));
    }
    let (pid, handle, base_addr, details) = crate::chains::connect(version)?;
    println!("Found {} {}", details.name, details.version.version);
    let config = crate::config::get_config();
    let mut handler =
        OverlayHandler::new_overlay_handler(&details.address_offsets, &details.arch, &base_addr, &handle, &pid, &config.overlay, &config.hotkeys)
            .context("This version has no overlay")?;
    handler.ui_memory_manager.get().settings.collision.set(true);
    handler.active = true;
    handler.handle_tick().map_err(|e| anyhow!("The update failed: {}", e))?;
    handler.active = false;
    let mut data = crate::ui_ipc_data::UiData::zeroed();
    handler.ui_memory_manager.get().read(&mut data);
    let capsule = |c: &Capsule| {
        format!("({:.0}, {:.0}, {:.0}) to ({:.0}, {:.0}, {:.0}) radius {:.1}", c.a.x, c.a.y, c.a.z, c.b.x, c.b.y, c.b.z, c.radius)
    };
    println!("Lara at ({:.0}, {:.0}, {:.0}), camera {}", data.position.x, data.position.y, data.position.z,
        if data.camera_view_address == 0 { "missing" } else { "found" });
    println!("{} objects", data.object_count);
    println!(
        "{} triggers ({} lines, {} faces), {} collision triangles, {} probes, {} hit capsules",
        data.trigger_count, data.trigger_line_count, data.trigger_face_count, data.triangle_count, data.probe_count,
        data.capsule_count
    );
    for label in data.triggers.iter().take((data.trigger_count as usize).min(5)) {
        println!("    {}", label.name().replace('\n', ": "));
    }
    println!("Lara's proxy shapes (pushing her: {}):", data.proxy_touching);
    for c in &data.proxies[..(data.proxy_count as usize).min(data.proxies.len())] {
        println!("    {}", capsule(c));
    }
    println!("Attached proxies (drawn: {}):", data.attached_active);
    for (c, touching) in data.attached[..(data.attached_count as usize).min(data.attached.len())].iter().zip(data.attached_touching) {
        println!("    {}{}", capsule(c), if touching { ", pushing her" } else { "" });
    }
    // Where her proxy's touching things, relative to her
    if let Some(proxy) = handler.game.lara_proxy(&mut handler.components, handler.lara_address) {
        let contacts = collision::read_contacts(handler.game.handle, proxy);
        println!("Contacts ({}), from her position:", contacts.len());
        for c in contacts {
            println!("    ({:.0}, {:.0}, {:.0})", c.x - data.position.x, c.y - data.position.y, c.z - data.position.z);
        }
    }
    Ok(())
}

impl Drop for OverlayHandler {
    fn drop(&mut self) {
        println!("Ejecting injected overlay dll");
        self.ui_memory_manager.get_mut().update(|val| val.should_exit = true);
        println!("done");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(labels: &[[u8; HOTKEY_LABEL_LENGTH]; HOTKEY_LABELS], control: HotkeyLabel) -> String {
        let bytes = &labels[control as usize];
        String::from_utf8_lossy(&bytes[..bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len())]).to_string()
    }

    #[test]
    fn controls_show_their_keys() {
        let config: crate::config::Config = serde_json::from_str("{}").unwrap();
        let labels = hotkey_labels(&config.hotkeys);
        assert_eq!(label(&labels, HotkeyLabel::Flight), "F5");
        assert_eq!(label(&labels, HotkeyLabel::Overlay), "F10");
        assert_eq!(label(&labels, HotkeyLabel::Window), "F11");
        assert_eq!(label(&labels, HotkeyLabel::Collision), "");

        let hotkeys: Vec<Hotkey> = serde_json::from_str(
            r#"[{"key": "F5", "action": {"ToggleActive": {}}}, {"key": "NumPad5", "action": {"ToggleActive": {}}}]"#,
        )
        .unwrap();
        assert_eq!(label(&hotkey_labels(&hotkeys), HotkeyLabel::Flight), "F5, NumPad5");
    }

    #[test]
    fn labels_go_at_the_middle_of_the_top() {
        let p = |x, y, z| Position { x, y, z };
        // A tilted edge and a low one
        let outline = vec![(p(0.0, 0.0, 0.0), p(10.0, 4.0, 30.0)), (p(-10.0, -4.0, 5.0), p(2.0, 0.0, 0.0))];
        let top = top_of(&outline).unwrap();
        assert_eq!((top.x, top.y, top.z), (0.0, 0.0, 30.0));
        assert!(top_of(&[]).is_none());
    }

    #[test]
    fn capsule_bounds_turn_with_the_object() {
        // Turned 90 degrees about z (x goes to y) and moved to (100, 0, 0)
        let transform = [0., 1., 0., 0., -1., 0., 0., 0., 0., 0., 1., 0., 100., 0., 0., 1.];
        // Lying along world y, i.e. the object's x
        let capsule = Capsule {
            a: Position { x: 100., y: -10., z: 0. },
            b: Position { x: 100., y: 10., z: 0. },
            radius: 2.,
        };
        let (min, max) = capsule_bounds(&[capsule], &transform).unwrap();
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(close(min.x, -12.) && close(max.x, 12.), "{:?} {:?}", min, max);
        assert!(close(min.y, -2.) && close(max.y, 2.), "{:?} {:?}", min, max);
        assert!(close(min.z, -2.) && close(max.z, 2.), "{:?} {:?}", min, max);
    }
}
