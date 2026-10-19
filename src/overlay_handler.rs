use crate::collision::{self, CapsuleShape};
use crate::collision_mesh::CollisionMeshes;
use crate::components::{self, Components};
use crate::config::OverlayConfig;
use crate::object_list::{self, Instance, ObjectList};
use crate::position::Position;
use crate::tracked_memory::TrackedMemory;
use crate::tracked_position::TrackedPosition;
use crate::action::Action;
use crate::handler::Handler;
use crate::inject32;
use crate::process_details::{AddressOffsets, AddressType};
use nalgebra::{Matrix3, Vector3};
use std::collections::HashMap;
use std::convert::TryInto;
use std::{env, fmt, io, time};
use std::error::Error;
use hudhook::inject::Process;
use process_memory::{Architecture, CopyAddress, DataMember, Memory, Pid, ProcessHandle};
use crate::trigger::{self, Shape};
use crate::trigger_scripts::{SetOffBy, TriggerScripts};
use crate::handler::HelperState;
use crate::ui_ipc_data::{REQUEST_RESTORE_POSITION, REQUEST_STORE_POSITION, REQUEST_TOGGLE_FLYING, name_bytes, text_bytes, trigger_action_bit, trigger_action_mask, projection_from_lens, BoxKind, Capsule, Line, ObjectBox, ObjectCategory, SharedMemoryManager, TriggerCategory, TriggerFace, TriggerLabel, UiSettings, MAX_ATTACHED, MAX_OBJECTS, MAX_TRIANGLES, MAX_TRIGGERS};

// How far the camera's ray is followed to find where Lara's aiming
const MAX_AIM_DISTANCE: f32 = 20000.0;
// How far above her feet the middle of Lara is (her standing capsule's centre), which counts as her being in a trigger
// as well as her feet
const LARA_MIDDLE: f32 = 85.0;
// Lara is the nearest instance with a PlayerComponent, out of this many
const LARA_CANDIDATES: usize = 8;
// How often to rescan the instance array for objects matching the filters
const OBJECT_REFRESH_INTERVAL: time::Duration = time::Duration::from_secs(1);
// Lara's contacts are often caught mid update, so keep showing an attached proxy as touching for this long
const TOUCHING_HOLD: time::Duration = time::Duration::from_millis(150);

pub struct OverlayHandler {
    active: bool,
    handle: ProcessHandle,
    pid: Pid,
    arch: Architecture,
    base_addr: usize,
    config: OverlayConfig,
    position: TrackedPosition,
    // The camera and objects, for drawing in the world, if this game has the offsets (TR2013 doesn't yet), else only
    // the window is shown
    camera_view: Option<TrackedCameraMatrix>,
    camera_lens: Option<TrackedMemory<[f32; 8]>>,
    // Camera to world, which starts at `CameraSin`
    camera_to_world: Option<TrackedCameraMatrix>,
    camera_view_offsets: Option<Vec<usize>>,
    camera_lens_offsets: Option<Vec<usize>>,
    instance_array_offsets: Option<Vec<usize>>,
    player_probes_offsets: Option<Vec<usize>>,
    // Lara's movement proxy, where it isn't found through her components (TR2013)
    player_proxy_offsets: Option<Vec<usize>>,
    combat_proxy_offsets: Option<Vec<usize>>,
    // TR2013's aim sweep: the instance array, inventory pool and type pool
    aim_sweep_offsets: Option<(Vec<usize>, Vec<usize>, Vec<usize>)>,
    collision_scene_offsets: Option<Vec<usize>>,
    collision_meshes: CollisionMeshes,
    // What the other handlers have to show, and actions the window's buttons asked for
    helper_state: HelperState,
    requested: Vec<Action>,
    // Coarse and detailed, and the kinds that are detailed, as of the last refresh
    mesh_settings: (bool, bool, u32),
    object_list: Option<ObjectList>,
    objects: Vec<(Instance, BoxKind, ObjectCategory)>,
    // Characters' and animals' hit capsules in bone space, by instance. Their models' bounds are often only part of
    // them (people are several models), so they're boxed round these instead.
    npc_capsules: HashMap<usize, Vec<CapsuleShape>>,
    // With their labels
    // With their labels, and the `TRIGGER_ACTIONS` index they're coloured by
    triggers: Vec<(Instance, Shape, TriggerCategory, String, u32)>,
    trigger_scripts: TriggerScripts,
    components: Components,
    // Found by her PlayerComponent each refresh
    lara_address: Option<usize>,
    // Interactables, triggers, trigger categories, NPCs, whether triggers are shown by action and which, as of the last
    // refresh
    object_settings: (bool, bool, u32, bool, bool, u32),
    proxy_last_touching: Option<time::Instant>,
    attached_last_touching: [Option<time::Instant>; MAX_ATTACHED],
    // Lara's instance and her capsules in bone space
    lara: Option<(usize, Vec<CapsuleShape>)>,
    last_object_refresh: Option<time::Instant>,
    ui_memory_manager: SharedMemoryManager,
}

impl OverlayHandler {
    pub fn new_overlay_handler(
        address_offsets: &AddressOffsets,
        arch: &Architecture,
        base_addr: &usize,
        handle: &ProcessHandle,
        pid: &Pid,
        config: &OverlayConfig,
    ) -> Option<OverlayHandler> {
        println!("Loading overlay handler...");

        let mut ui_memory_manager = SharedMemoryManager::new(true);
        // Kept when the handler is recreated (e.g. when the offsets change)
        let settings = &ui_memory_manager.get().settings;
        if !UiSettings::get(&settings.initialised) {
            UiSettings::set(&settings.collision, config.collision);
            UiSettings::set(&settings.lara_collision, true);
            UiSettings::set(&settings.coarse_meshes, config.collision_meshes);
            UiSettings::set(&settings.detailed_meshes, config.detailed_collision_meshes);
            UiSettings::set_u32(&settings.other_kinds, config.collision_mesh_other_kinds);
            UiSettings::set_f32(&settings.coarse_radius, config.collision_mesh_radius);
            UiSettings::set_f32(&settings.detailed_radius, config.detailed_collision_mesh_radius);
            UiSettings::set_f32(&settings.mesh_opacity, config.collision_opacity);
            UiSettings::set_f32(&settings.trigger_opacity, config.trigger_opacity);
            UiSettings::set(&settings.fade_with_distance, config.collision_mesh_fade);
            UiSettings::set_f32(&settings.fade_strength, config.collision_mesh_fade_strength);
            UiSettings::set(&settings.cull_hidden, config.collision_mesh_cull_hidden);
            UiSettings::set(&settings.follow_aim, config.collision_mesh_follow_aim);
            UiSettings::set(&settings.interactables, config.interactables);
            UiSettings::set(&settings.triggers, config.triggers);
            UiSettings::set(&settings.npcs, config.npcs);
            UiSettings::set_u32(&settings.trigger_categories, TriggerCategory::mask(&config.trigger_categories));
            UiSettings::set(&settings.triggers_by_action, config.triggers_by_action);
            UiSettings::set_u32(&settings.trigger_actions, trigger_action_mask(&config.trigger_actions));
            UiSettings::set(&settings.show_window, config.settings_window);
            UiSettings::set(&settings.initialised, true);
        }
        ui_memory_manager.get_mut().update(|val| {
            val.debug = config.console;
            val.should_exit = false;
            val.object_count = 0;
            val.triangle_count = 0;
        });

        Some(OverlayHandler {
            active: false,
            handle: *handle,
            pid: *pid,
            arch: *arch,
            base_addr: *base_addr,
            config: config.clone(),
            position: TrackedPosition::new(
                address_offsets.get(&AddressType::XPosition)?.clone(),
                address_offsets.get(&AddressType::YPosition)?.clone(),
                address_offsets.get(&AddressType::ZPosition)?.clone(),
                *arch,
                *base_addr,
            ),
            camera_view: address_offsets
                .get(&AddressType::CameraView)
                .map(|offsets| TrackedCameraMatrix::new(offsets.clone(), *arch, *base_addr)),
            camera_lens: address_offsets
                .get(&AddressType::CameraLens)
                .map(|offsets| TrackedMemory::new([0.0; 8], offsets.clone(), *arch, *base_addr)),
            camera_to_world: address_offsets
                .get(&AddressType::CameraSin)
                .map(|offsets| TrackedCameraMatrix::new(offsets.clone(), *arch, *base_addr)),
            camera_view_offsets: address_offsets.get(&AddressType::CameraView).cloned(),
            camera_lens_offsets: address_offsets.get(&AddressType::CameraLens).cloned(),
            instance_array_offsets: address_offsets.get(&AddressType::InstanceArray).cloned(),
            player_probes_offsets: address_offsets.get(&AddressType::PlayerProbes).cloned(),
            player_proxy_offsets: address_offsets.get(&AddressType::PlayerProxy).cloned(),
            combat_proxy_offsets: address_offsets.get(&AddressType::CombatProxy).cloned(),
            aim_sweep_offsets: match (
                address_offsets.get(&AddressType::InstanceArray),
                address_offsets.get(&AddressType::InventoryPool),
                address_offsets.get(&AddressType::TypePool),
            ) {
                (Some(i), Some(p), Some(t)) => Some((i.clone(), p.clone(), t.clone())),
                _ => None,
            },
            collision_scene_offsets: address_offsets.get(&AddressType::CollisionScene).cloned(),
            collision_meshes: CollisionMeshes::default(),
            mesh_settings: (false, false, 0),
            helper_state: HelperState::default(),
            requested: vec![],
            object_list: None,
            objects: vec![],
            npc_capsules: HashMap::new(),
            triggers: vec![],
            trigger_scripts: TriggerScripts::new(*pid, *handle, *base_addr),
            components: Components::new(*base_addr),
            lara_address: None,
            object_settings: (false, false, 0, false, false, 0),
            lara: None,
            last_object_refresh: None,
            proxy_last_touching: None,
            attached_last_touching: [None; MAX_ATTACHED],
            ui_memory_manager,
        })
    }

    // TR2013's aim sweep, found from Lara's proxy, and whether it's hitting something
    fn read_aim_sweep(&self, proxy: usize) -> Option<(Capsule, bool)> {
        let (instances, inventories, types) = self.aim_sweep_offsets.as_ref()?;
        let mut lara = vec![0u8; crate::layout::pointer_size()];
        self.handle.copy_address(proxy + crate::layout::current().aim_sweep.proxy_instance, &mut lara).ok()?;
        crate::aim_sweep::read(
            self.handle,
            self.resolve(instances).ok()?,
            self.resolve(inventories).ok()?,
            self.resolve(types).ok()?,
            crate::layout::pointer_from(&lara),
        )
    }

    fn resolve(&self, offsets: &Vec<usize>) -> io::Result<usize> {
        let mut offsets = offsets.clone();
        offsets[0] += self.base_addr;
        DataMember::<u8>::new_offset(self.handle, offsets)
            .set_arch(self.arch)
            .get_offset()
    }

    fn refresh_objects(
        &mut self,
        interactables: bool,
        triggers: bool,
        categories: u32,
        npcs: bool,
        by_action: bool,
        actions: u32,
    ) -> Result<(), Box<dyn Error>> {
        if self.object_list.is_none() {
            let offsets = match &self.instance_array_offsets {
                Some(offsets) => offsets.clone(),
                None => return Ok(()),
            };
            self.object_list = Some(ObjectList::new(self.resolve(&offsets)?));
        }

        let filters: Vec<String> = self.config.object_filters.iter().map(|f| f.to_lowercase()).collect();
        let lara = self.position.to_position();
        let distance = |p: &Position| {
            ((p.x - lara.x).powi(2) + (p.y - lara.y).powi(2) + (p.z - lara.z).powi(2)).sqrt()
        };

        let all = self.object_list.as_mut().unwrap().instances(self.handle)?;
        let names: HashMap<u64, String> = all.iter().map(|i| (i.id, i.name.clone())).collect();
        let mut nearby: Vec<Instance> =
            all.into_iter().filter(|i| distance(&i.position) <= self.config.object_radius).collect();
        nearby.sort_by(|a, b| distance(&a.position).total_cmp(&distance(&b.position)));

        // She has her own collision drawn, and counts as interactable herself (she has an InteractableComponent). She's
        // the nearest instance with a PlayerComponent.
        self.components.start_refresh();
        let handle = self.handle;
        let components = &mut self.components;
        let lara_address = nearby
            .iter()
            .take(LARA_CANDIDATES)
            .map(|i| i.address)
            .find(|&address| components.get(handle, address, components::PLAYER).is_some());
        self.lara_address = lara_address;

        let mut objects = vec![];
        let mut trigger_shapes = vec![];
        for instance in nearby {
            let name = instance.name.to_lowercase();
            let full_name = instance.full_name.to_lowercase();
            let named = filters.iter().any(|f| name.contains(f.as_str()) || full_name.contains(f.as_str()));
            let kinds = if interactables || triggers || npcs {
                trigger::read_kinds(self.handle, &mut self.components, instance.address)
            } else {
                trigger::Kinds::default()
            };
            let category = trigger::categorise(&instance.name, &instance.full_name);
            // By what they do once that's known, below
            let wanted = by_action || categories & category.bit() != 0;
            if triggers && kinds.trigger && wanted {
                trigger_shapes.push((instance.clone(), category));
            }
            let is_lara = Some(instance.address) == lara_address;
            if named {
                objects.push((instance, ObjectCategory::Named));
            } else if is_lara {
                continue;
            } else if kinds.npc {
                // They have interaction components too (for looting them)
                if npcs {
                    objects.push((instance, ObjectCategory::Npc));
                }
            } else if interactables && kinds.interactable && trigger::for_lara(&instance.name) {
                objects.push((instance, ObjectCategory::Interactable));
            }
        }
        // What sets them off is found in the background, so it can change from "unchecked" between refreshes
        let ids: Vec<u64> = trigger_shapes.iter().map(|(i, _)| i.id).collect();
        if triggers {
            self.trigger_scripts.update(&ids, &names);
        }
        let mut shown = vec![];
        for (instance, category) in trigger_shapes {
            let script = self.trigger_scripts.script(instance.id);
            let by = match script.map(|s| &s.set_off_by) {
                Some(SetOffBy::Lara) => Some("Lara".to_string()),
                Some(SetOffBy::Instance(name)) => Some(name.clone().unwrap_or_else(|| "an instance".to_string())),
                Some(SetOffBy::Script) => Some("script".to_string()),
                Some(SetOffBy::Nothing) => Some("no script".to_string()),
                None => None,
            };
            let does = script.map_or(trigger_action_bit("unchecked"), |s| s.actions);
            if (by_action && actions & does == 0) || shown.len() >= MAX_TRIGGERS {
                continue;
            }
            if let Some(shape) = trigger::read_shape(self.handle, &mut self.components, instance.address) {
                let mut label = match by {
                    Some(by) => format!("{} ({})", instance.name, by),
                    None => instance.name.clone(),
                };
                // What it does on a second line
                if let Some(script) = script.filter(|s| !s.does.is_empty()) {
                    label = format!("{}\n{}", label, script.does.join(", "));
                }
                // The first of what it does that's shown (all of it when shown by name)
                let shown_does = if by_action { does & actions } else { does };
                shown.push((instance, shape, category, label, shown_does.trailing_zeros()));
            }
        }
        self.triggers = shown;

        let mut boxes = vec![];
        self.lara = None;
        if let (true, Some(address)) = (self.config.lara_capsules, lara_address) {
            let shapes = collision::read_capsule_shapes(self.handle, &mut self.components, address)?;
            if shapes.is_empty() {
                // Better than nothing
                boxes.push((
                    Instance {
                        address,
                        id: 0,
                        position: object_list::read_position(self.handle, address)?,
                        name: "Lara".to_string(),
                        full_name: String::new(),
                        bounds: None,
                    },
                    BoxKind::Lara,
                    ObjectCategory::Named,
                ));
            } else {
                self.lara = Some((address, shapes));
            }
        }
        boxes.extend(objects.into_iter().map(|(o, category)| (o, BoxKind::Model, category)));
        boxes.truncate(MAX_OBJECTS);

        // Models can change (e.g. Lara's outfit), so these are refreshed with the list
        self.npc_capsules.clear();
        for (object, kind, category) in &mut boxes {
            object.bounds = object_list::read_bounds(self.handle, &mut self.components, object.address);
            if object.bounds.is_none() && *kind == BoxKind::Model {
                *kind = BoxKind::Estimated;
            }
            if *category == ObjectCategory::Npc {
                if let Ok(shapes) = collision::read_capsule_shapes(self.handle, &mut self.components, object.address) {
                    if !shapes.is_empty() {
                        self.npc_capsules.insert(object.address, shapes);
                    }
                }
            }
        }
        self.objects = boxes;
        Ok(())
    }
}

// The box round the (world space) capsules in the space of the object-to-world `transform` (row-major, row
// vectors), so it turns with the object. None if there aren't any capsules, or the transform can't be inverted.
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
            let point = to_object * (Vector3::new(end.x, end.y, end.z) - origin);
            let (low, high) = (point - radius, point + radius);
            bounds = Some(match bounds {
                Some((min, max)) => (min.inf(&low), max.sup(&high)),
                None => (low, high),
            });
        }
    }
    let (min, max) = bounds?;
    let position = |v: Vector3<f32>| Position { x: v.x, y: v.y, z: v.z };
    Some((position(min), position(max)))
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

        self.position.fetch_from_game(self.handle)?;
        if let Some(camera_view) = &mut self.camera_view {
            camera_view.fetch_from_game(self.handle)?;
        }
        if let Some(camera_lens) = &mut self.camera_lens {
            camera_lens.fetch_from_game(self.handle)?;
        }

        let settings = &self.ui_memory_manager.get().settings;
        let requests = settings.requests.swap(0, std::sync::atomic::Ordering::SeqCst);
        for (bit, action) in [
            (REQUEST_STORE_POSITION, Action::StorePosition {}),
            (REQUEST_RESTORE_POSITION, Action::RestorePosition {}),
            (REQUEST_TOGGLE_FLYING, Action::ToggleActive {}),
        ] {
            if requests & bit != 0 {
                self.requested.push(action);
            }
        }
        let collision = UiSettings::get(&settings.collision);
        let lara_collision = collision && UiSettings::get(&settings.lara_collision);
        let mesh_settings = (
            collision && UiSettings::get(&settings.coarse_meshes),
            collision && UiSettings::get(&settings.detailed_meshes),
            UiSettings::get_u32(&settings.other_kinds),
        );
        let show_meshes = mesh_settings.0 || mesh_settings.1;
        let follow_aim = show_meshes && UiSettings::get(&settings.follow_aim);
        let coarse_radius = UiSettings::get_f32(&settings.coarse_radius);
        let detailed_radius = UiSettings::get_f32(&settings.detailed_radius);

        let object_settings = (
            UiSettings::get(&settings.interactables),
            UiSettings::get(&settings.triggers),
            UiSettings::get_u32(&settings.trigger_categories),
            UiSettings::get(&settings.npcs),
            UiSettings::get(&settings.triggers_by_action),
            UiSettings::get_u32(&settings.trigger_actions),
        );

        let refresh_due = self.last_object_refresh.map_or(true, |t| t.elapsed() > OBJECT_REFRESH_INTERVAL);
        // Straight away when they're toggled too
        if refresh_due || object_settings != self.object_settings {
            self.last_object_refresh = Some(time::Instant::now());
            self.object_settings = object_settings;
            let (interactables, triggers, categories, npcs, by_action, actions) = object_settings;
            self.refresh_objects(interactables, triggers, categories, npcs, by_action, actions)?;
        }
        // Straight away when they're toggled
        if refresh_due || mesh_settings != self.mesh_settings {
            self.mesh_settings = mesh_settings;
            if let (true, Some(offsets)) = (show_meshes, &self.collision_scene_offsets) {
                let (coarse, detailed, other_kinds) = mesh_settings;
                self.collision_meshes.refresh(self.handle, self.resolve(offsets)?, coarse, detailed, other_kinds)?;
            }
        }

        let probes = match (lara_collision && self.config.lara_probes, &self.player_probes_offsets) {
            (true, Some(offsets)) => {
                let probes = collision::read_probes(self.handle, self.resolve(offsets)?)?;
                collision::probes_near(&probes, &self.position.to_position())
            }
            _ => vec![],
        };
        // Lara can be reloaded (and moved) between refreshes, which `components.get` notices. Her attached proxies being
        // used is also the best sign she's aiming.
        let show_proxy = lara_collision && self.config.lara_proxy;
        let proxies = match (show_proxy || follow_aim, &self.player_proxy_offsets, self.lara_address) {
            (true, Some(offsets), _) => {
                // The combat proxy is found on its own too
                let combat: Vec<usize> =
                    self.combat_proxy_offsets.as_ref().and_then(|offsets| self.resolve(offsets).ok()).into_iter().collect();
                let proxy = self.resolve(offsets).ok();
                let mut proxies = proxy.and_then(|proxy| collision::read_proxy(self.handle, proxy, &combat).ok());
                // The aim sweep is drawn like the proxies attached to hers
                if let (Some(proxies), Some(rod)) = (&mut proxies, proxy.and_then(|p| self.read_aim_sweep(p))) {
                    proxies.attached.push(rod);
                }
                proxies
            }
            (true, None, Some(lara)) => self
                .components
                .get(self.handle, lara, components::PROXY)
                .and_then(|proxy| collision::read_proxy(self.handle, proxy, &[]).ok()),
            _ => None,
        };
        let aiming = proxies.as_ref().map_or(false, |p| p.attached_active);
        let (proxies, proxy_touching, attached, attached_active) = match proxies {
            Some(p) if show_proxy => (p.shapes, p.touching, p.attached, p.attached_active),
            _ => (vec![], false, vec![], false),
        };
        if proxy_touching {
            self.proxy_last_touching = Some(time::Instant::now());
        }

        let size = self.config.object_box_size;
        let mut objects = vec![];
        for (object, kind, category) in &self.objects {
            // Objects can be unloaded between refreshes
            let transform = match object_list::read_transform(self.handle, object.address) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let name = name_bytes(&object.name);
            let (min, max) = object.bounds.unwrap_or((
                Position { x: -size, y: -size, z: -size },
                Position { x: size, y: size, z: size },
            ));
            // Round the capsules where they are now
            let placed = self
                .npc_capsules
                .get(&object.address)
                .and_then(|shapes| collision::place_capsules(self.handle, object.address, shapes).ok())
                .and_then(|capsules| capsule_bounds(&capsules, &transform));
            let (min, max) = placed.unwrap_or((min, max));
            objects.push(ObjectBox {
                transform,
                min,
                max,
                kind: *kind,
                category: *category,
                name,
            });
        }

        let mut trigger_labels = vec![];
        let lara = self.position.to_position();
        let mut trigger_lines = vec![];
        let mut trigger_faces = vec![];
        for (trigger, shape, category, label, action) in &self.triggers {
            let transform = match object_list::read_transform(self.handle, trigger.address) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let outline = trigger::outline(shape, &transform);
            let origin = Position { x: transform[12], y: transform[13], z: transform[14] };
            trigger_labels.push(TriggerLabel {
                position: top_of(&outline).unwrap_or(origin),
                category: *category,
                action: *action,
                name: text_bytes(label),
            });
            let inside = [0.0, LARA_MIDDLE]
                .iter()
                .any(|up| trigger::contains(shape, &transform, &Position { z: lara.z + up, ..lara }));
            trigger_lines.extend(outline.into_iter().map(|(a, b)| Line { a, b, category: *category, action: *action, inside }));
            let faces = trigger::faces(shape, &transform);
            trigger_faces.extend(faces.into_iter().map(|corners| TriggerFace { corners, category: *category, action: *action }));
        }

        let proxy_touching = self.proxy_last_touching.map_or(false, |t| t.elapsed() < TOUCHING_HOLD);
        let mut attached_touching = [false; MAX_ATTACHED];
        for (i, (_, touching)) in attached.iter().take(MAX_ATTACHED).enumerate() {
            if *touching {
                self.attached_last_touching[i] = Some(time::Instant::now());
            }
            attached_touching[i] = self.attached_last_touching[i].map_or(false, |t| t.elapsed() < TOUCHING_HOLD);
        }
        let attached: Vec<_> = attached.iter().map(|(capsule, _)| *capsule).collect();

        let capsules = match (lara_collision, &self.lara) {
            (true, Some((address, shapes))) => collision::place_capsules(self.handle, *address, shapes)?,
            _ => vec![],
        };

        let position = self.position.to_position();
        let started = time::Instant::now();
        // Where the camera's looking (the reticle's in the middle of the screen), while Lara aims
        let camera = match (follow_aim && aiming, &mut self.camera_to_world) {
            (true, Some(camera)) => camera.fetch_from_game(self.handle).ok().map(|_| camera.matrix),
            _ => None,
        };
        let aim_point = camera.and_then(|m| {
            // Its last row is where it is, the one before which way it faces
            let origin = Vector3::new(m[12], m[13], m[14]);
            let forward = Vector3::new(m[8], m[9], m[10]).try_normalize(1e-6)?;
            let hit = self.collision_meshes.raycast(self.handle, origin, forward, MAX_AIM_DISTANCE)?;
            let point = origin + forward * hit;
            Some(Position { x: point.x, y: point.y, z: point.z })
        });
        let mesh_centre = aim_point.unwrap_or(position);
        let triangles = if show_meshes {
            self.collision_meshes.triangles_near(self.handle, &mesh_centre, coarse_radius, detailed_radius, MAX_TRIANGLES)
        } else {
            vec![]
        };
        let triangle_milliseconds = started.elapsed().as_secs_f32() * 1000.0;

        let camera_view = self.camera_view.as_ref().map_or([0.0; 16], |c| c.to_array());
        let camera = &crate::layout::current().camera;
        let lens_fields = [camera.lens_near, camera.lens_far, camera.lens_fov, camera.lens_aspect].map(|f| f as u32);
        let camera_projection = self.camera_lens.as_ref().map_or([0.0; 16], |l| projection_from_lens(&l.data, lens_fields));
        // 0 tells the overlay there's no camera
        let address = |offsets: &Option<Vec<usize>>| -> io::Result<u64> {
            offsets.as_ref().map_or(Ok(0), |offsets| self.resolve(offsets).map(|a| a as u64))
        };
        let camera_view_address = address(&self.camera_view_offsets)?;
        let camera_lens_address = address(&self.camera_lens_offsets)?;
        let kind_counts = self.collision_meshes.kind_counts;
        let helper_state = self.helper_state.clone();
        self.ui_memory_manager.get_mut().update(|val| {
            val.position_tools = helper_state.position_tools;
            val.has_saved_position = helper_state.saved_position.is_some();
            val.saved_position = helper_state.saved_position.unwrap_or_default();
            val.flying = helper_state.flying;
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
            val.proxy_count = fill(&mut val.proxies, &proxies);
            val.proxy_touching = proxy_touching;
            val.attached_count = fill(&mut val.attached, &attached);
            val.attached_touching = attached_touching;
            val.attached_active = attached_active;
            val.capsule_count = fill(&mut val.capsules, &capsules);
            val.trigger_count = fill(&mut val.triggers, &trigger_labels);
            val.trigger_line_count = fill(&mut val.trigger_lines, &trigger_lines);
            val.trigger_face_count = fill(&mut val.trigger_faces, &trigger_faces);
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
            Action::ToggleLaraCollision {} => UiSettings::toggle(&self.ui_memory_manager.get().settings.lara_collision),
            Action::ToggleCoarseCollision {} => UiSettings::toggle(&self.ui_memory_manager.get().settings.coarse_meshes),
            Action::ToggleDetailedCollision {} => UiSettings::toggle(&self.ui_memory_manager.get().settings.detailed_meshes),
            Action::ToggleInteractables {} => UiSettings::toggle(&self.ui_memory_manager.get().settings.interactables),
            Action::ToggleTriggers {} => UiSettings::toggle(&self.ui_memory_manager.get().settings.triggers),
            Action::ToggleNpcs {} => UiSettings::toggle(&self.ui_memory_manager.get().settings.npcs),
            Action::ToggleCollision {} => UiSettings::toggle(&self.ui_memory_manager.get().settings.collision),
            Action::ToggleSettingsWindow {} => UiSettings::toggle(&self.ui_memory_manager.get().settings.show_window),
            _ => (),
        }
        Ok(())
    }
}

/// `overlay-data`: what the overlay would be sent now, from one update with collision on, without injecting it
pub fn print_overlay_data(version: Option<String>, delay: u64) -> Result<(), String> {
    if delay > 0 {
        println!("Starting in {} seconds...", delay);
        std::thread::sleep(time::Duration::from_secs(delay));
    }
    let (pid, handle, base_addr, details) = crate::chains::connect(version)?;
    println!("Found {} {}", details.name, details.version.version);
    let config = crate::config::get_config();
    let mut handler =
        OverlayHandler::new_overlay_handler(&details.address_offsets, &details.arch, &base_addr, &handle, &pid, &config.overlay)
            .ok_or("This version has no overlay")?;
    UiSettings::set(&handler.ui_memory_manager.get().settings.collision, true);
    handler.active = true;
    handler.handle_tick().map_err(|e| format!("The update failed: {}", e))?;
    handler.active = false;
    let mut data = crate::ui_ipc_data::UiData::zeroed();
    handler.ui_memory_manager.get().read(&mut data);
    let capsule = |c: &Capsule| {
        format!("({:.0}, {:.0}, {:.0}) to ({:.0}, {:.0}, {:.0}) radius {:.1}", c.a.x, c.a.y, c.a.z, c.b.x, c.b.y, c.b.z, c.radius)
    };
    println!("Lara at ({:.0}, {:.0}, {:.0}), camera {}", data.position.x, data.position.y, data.position.z,
        if data.camera_view_address == 0 { "missing" } else { "found" });
    println!("{} objects", data.object_count);
    println!("Lara's proxy shapes (pushing her: {}):", data.proxy_touching);
    for c in &data.proxies[..(data.proxy_count as usize).min(data.proxies.len())] {
        println!("    {}", capsule(c));
    }
    println!("Attached proxies (drawn: {}):", data.attached_active);
    for (c, touching) in data.attached[..(data.attached_count as usize).min(data.attached.len())].iter().zip(data.attached_touching) {
        println!("    {}{}", capsule(c), if touching { ", pushing her" } else { "" });
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

#[derive(Debug, Clone)]
pub struct TrackedCameraMatrix {
    pub matrix: [f32; 16],
    offsets: Vec<usize>,
    arch: Architecture,
    base_addr: usize,
}

impl TrackedCameraMatrix {
    // Reads the whole matrix at once, so it isn't a mix of two game frames
    pub fn fetch_from_game(&mut self, handle: ProcessHandle) -> io::Result<()> {
        let mut offsets = self.offsets.clone();
        offsets[0] += self.base_addr;
        let address = DataMember::<u8>::new_offset(handle, offsets)
            .set_arch(self.arch)
            .get_offset()?;
        let mut bytes = [0u8; 64];
        handle.copy_address(address, &mut bytes)?;
        for (i, element) in self.matrix.iter_mut().enumerate() {
            *element = f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        }
        Ok(())
    }

    pub fn new(
        matrix_offsets: Vec<usize>,
        arch: Architecture,
        base_addr: usize,
    ) -> TrackedCameraMatrix {
        TrackedCameraMatrix {
            matrix: [0.0; 16],
            offsets: matrix_offsets,
            arch,
            base_addr,
        }
    }

    pub fn to_array(&self) -> [f32; 16] {
        self.matrix
    }
}

impl fmt::Display for TrackedCameraMatrix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for row in self.matrix.chunks(4) {
            writeln!(f, "({}, {}, {}, {})", row[0], row[1], row[2], row[3])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
