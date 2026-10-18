use crate::collision::{self, CapsuleShape};
use crate::collision_mesh::CollisionMeshes;
use crate::config::OverlayConfig;
use crate::object_list::{self, Instance, ObjectList};
use crate::position::Position;
use crate::tracked_memory::TrackedMemory;
use crate::tracked_position::TrackedPosition;
use crate::action::Action;
use crate::handler::Handler;
use crate::process_details::{AddressOffsets, AddressType};
use nalgebra::{Matrix3, Vector3};
use std::collections::HashMap;
use std::convert::TryInto;
use std::{env, fmt, io, time};
use std::error::Error;
use hudhook::inject::Process;
use process_memory::{Architecture, CopyAddress, DataMember, Memory, Pid, ProcessHandle};
use crate::trigger::{self, Shape};
use crate::ui_ipc_data::{name_bytes, projection_from_lens, BoxKind, Capsule, Line, ObjectBox, ObjectCategory, SharedMemoryManager, TriggerCategory, TriggerLabel, UiSettings, MAX_ATTACHED, MAX_OBJECTS, MAX_TRIANGLES, MAX_TRIGGERS};

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
    camera_view: TrackedCameraMatrix,
    camera_lens: TrackedMemory<[f32; 8]>,
    camera_view_offsets: Vec<usize>,
    camera_lens_offsets: Vec<usize>,
    instance_array_offsets: Vec<usize>,
    player_instance_offsets: Option<Vec<usize>>,
    player_probes_offsets: Option<Vec<usize>>,
    proxy_type_offsets: Option<Vec<usize>>,
    collision_scene_offsets: Option<Vec<usize>>,
    collision_meshes: CollisionMeshes,
    // Coarse and detailed, as of the last refresh
    mesh_settings: (bool, bool),
    object_list: Option<ObjectList>,
    objects: Vec<(Instance, BoxKind, ObjectCategory)>,
    // Characters' and animals' hit capsules in bone space, by instance. Their models' bounds are often only part of
    // them (people are several models), so they're boxed round these instead.
    npc_capsules: HashMap<usize, Vec<CapsuleShape>>,
    triggers: Vec<(Instance, Shape, TriggerCategory)>,
    classes: trigger::Classes,
    // Interactables, triggers, trigger categories and NPCs, as of the last refresh
    object_settings: (bool, bool, u32, bool),
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
            UiSettings::set(&settings.lara_collision, true);
            UiSettings::set(&settings.coarse_meshes, config.collision_meshes);
            UiSettings::set(&settings.detailed_meshes, config.detailed_collision_meshes);
            UiSettings::set_f32(&settings.coarse_radius, config.collision_mesh_radius);
            UiSettings::set_f32(&settings.detailed_radius, config.detailed_collision_mesh_radius);
            UiSettings::set_f32(&settings.mesh_opacity, config.collision_opacity);
            UiSettings::set(&settings.fade_with_distance, config.collision_mesh_fade);
            UiSettings::set_f32(&settings.fade_strength, config.collision_mesh_fade_strength);
            UiSettings::set(&settings.cull_hidden, config.collision_mesh_cull_hidden);
            UiSettings::set(&settings.interactables, config.interactables);
            UiSettings::set(&settings.triggers, config.triggers);
            UiSettings::set(&settings.npcs, config.npcs);
            UiSettings::set_u32(&settings.trigger_categories, TriggerCategory::mask(&config.trigger_categories));
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
            camera_view: TrackedCameraMatrix::new(
                address_offsets.get(&AddressType::CameraView)?.clone(),
                *arch,
                *base_addr,
            ),
            camera_lens: TrackedMemory::new(
                [0.0; 8],
                address_offsets.get(&AddressType::CameraLens)?.clone(),
                *arch,
                *base_addr,
            ),
            camera_view_offsets: address_offsets.get(&AddressType::CameraView)?.clone(),
            camera_lens_offsets: address_offsets.get(&AddressType::CameraLens)?.clone(),
            instance_array_offsets: address_offsets.get(&AddressType::InstanceArray)?.clone(),
            player_instance_offsets: address_offsets.get(&AddressType::PlayerInstance).cloned(),
            player_probes_offsets: address_offsets.get(&AddressType::PlayerProbes).cloned(),
            proxy_type_offsets: address_offsets.get(&AddressType::ProxyComponentType).cloned(),
            collision_scene_offsets: address_offsets.get(&AddressType::CollisionScene).cloned(),
            collision_meshes: CollisionMeshes::default(),
            mesh_settings: (false, false),
            object_list: None,
            objects: vec![],
            npc_capsules: HashMap::new(),
            triggers: vec![],
            classes: trigger::Classes::new(*base_addr),
            object_settings: (false, false, 0, false),
            lara: None,
            last_object_refresh: None,
            proxy_last_touching: None,
            attached_last_touching: [None; MAX_ATTACHED],
            ui_memory_manager,
        })
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
    ) -> Result<(), Box<dyn Error>> {
        if self.object_list.is_none() {
            self.object_list = Some(ObjectList::new(self.resolve(&self.instance_array_offsets)?));
        }

        let filters: Vec<String> = self.config.object_filters.iter().map(|f| f.to_lowercase()).collect();
        let lara = self.position.to_position();
        let distance = |p: &Position| {
            ((p.x - lara.x).powi(2) + (p.y - lara.y).powi(2) + (p.z - lara.z).powi(2)).sqrt()
        };

        let mut nearby: Vec<Instance> = self.object_list.as_mut().unwrap()
            .instances(self.handle)?
            .into_iter()
            .filter(|i| distance(&i.position) <= self.config.object_radius)
            .collect();
        nearby.sort_by(|a, b| distance(&a.position).total_cmp(&distance(&b.position)));

        // She has her own collision drawn, and counts as interactable herself (she has an InteractableComponent)
        let lara_address = match &self.player_instance_offsets {
            Some(offsets) => self.resolve(offsets).ok(),
            None => None,
        };

        let mut objects = vec![];
        let mut trigger_shapes = vec![];
        for instance in nearby {
            let name = instance.name.to_lowercase();
            let full_name = instance.full_name.to_lowercase();
            let named = filters.iter().any(|f| name.contains(f.as_str()) || full_name.contains(f.as_str()));
            let kinds = if interactables || triggers || npcs {
                trigger::read_kinds(self.handle, &mut self.classes, instance.address)
            } else {
                trigger::Kinds::default()
            };
            let category = trigger::categorise(&instance.name, &instance.full_name);
            let wanted = categories & category.bit() != 0;
            if triggers && kinds.trigger && wanted && trigger_shapes.len() < MAX_TRIGGERS {
                if let Some(shape) = trigger::read_shape(self.handle, &mut self.classes, instance.address) {
                    trigger_shapes.push((instance.clone(), shape, category));
                }
            }
            let is_lara = Some(instance.address) == lara_address;
            if named {
                objects.push((instance, ObjectCategory::Named));
            } else if is_lara {
                continue;
            } else if npcs && kinds.npc {
                objects.push((instance, ObjectCategory::Npc));
            } else if interactables && (kinds.interactable || trigger::is_resource(&instance.name)) {
                objects.push((instance, ObjectCategory::Interactable));
            }
        }
        self.triggers = trigger_shapes;

        let mut boxes = vec![];
        self.lara = None;
        if let (true, Some(offsets)) = (self.config.lara_capsules, &self.player_instance_offsets) {
            let address = self.resolve(offsets)?;
            let shapes = collision::read_capsule_shapes(self.handle, address)?;
            if shapes.is_empty() {
                // Better than nothing
                boxes.push((
                    Instance {
                        address,
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
            object.bounds = object_list::read_bounds(self.handle, object.address);
            if object.bounds.is_none() && *kind == BoxKind::Model {
                *kind = BoxKind::Estimated;
            }
            if *category == ObjectCategory::Npc {
                if let Ok(shapes) = collision::read_capsule_shapes(self.handle, object.address) {
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
fn fill<T: Copy>(slots: &mut [T], items: &[T]) -> usize {
    let count = items.len().min(slots.len());
    slots[..count].copy_from_slice(&items[..count]);
    count
}

impl Handler for OverlayHandler {
    fn handle_tick(&mut self) -> Result<(), Box<dyn Error>> {
        if !self.active {
            return Ok(());
        }

        self.position.fetch_from_game(self.handle)?;
        self.camera_view.fetch_from_game(self.handle)?;
        self.camera_lens.fetch_from_game(self.handle)?;

        let settings = &self.ui_memory_manager.get().settings;
        let lara_collision = UiSettings::get(&settings.lara_collision);
        let mesh_settings = (UiSettings::get(&settings.coarse_meshes), UiSettings::get(&settings.detailed_meshes));
        let show_meshes = mesh_settings.0 || mesh_settings.1;
        let coarse_radius = UiSettings::get_f32(&settings.coarse_radius);
        let detailed_radius = UiSettings::get_f32(&settings.detailed_radius);

        let object_settings = (
            UiSettings::get(&settings.interactables),
            UiSettings::get(&settings.triggers),
            UiSettings::get_u32(&settings.trigger_categories),
            UiSettings::get(&settings.npcs),
        );

        let refresh_due = self.last_object_refresh.map_or(true, |t| t.elapsed() > OBJECT_REFRESH_INTERVAL);
        // Straight away when they're toggled too
        if refresh_due || object_settings != self.object_settings {
            self.last_object_refresh = Some(time::Instant::now());
            self.object_settings = object_settings;
            let (interactables, triggers, categories, npcs) = object_settings;
            self.refresh_objects(interactables, triggers, categories, npcs)?;
        }
        // Straight away when they're toggled
        if refresh_due || mesh_settings != self.mesh_settings {
            self.mesh_settings = mesh_settings;
            if let (true, Some(offsets)) = (show_meshes, &self.collision_scene_offsets) {
                let (coarse, detailed) = mesh_settings;
                self.collision_meshes.refresh(self.handle, self.resolve(offsets)?, coarse, detailed)?;
            }
        }

        let probes = match (lara_collision && self.config.lara_probes, &self.player_probes_offsets) {
            (true, Some(offsets)) => collision::read_probes(self.handle, self.resolve(offsets)?)?,
            _ => vec![],
        };
        let proxies = match (lara_collision && self.config.lara_proxy, &self.player_instance_offsets, &self.proxy_type_offsets) {
            (true, Some(instance), Some(type_id)) => {
                Some(collision::read_proxy(self.handle, self.resolve(instance)?, self.resolve(type_id)?)?)
            }
            _ => None,
        };
        let (proxies, proxy_touching, attached, attached_active) = match proxies {
            Some(p) => (p.shapes, p.touching, p.attached, p.attached_active),
            None => (vec![], false, vec![], false),
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
        let mut trigger_lines = vec![];
        for (trigger, shape, category) in &self.triggers {
            let transform = match object_list::read_transform(self.handle, trigger.address) {
                Ok(t) => t,
                Err(_) => continue,
            };
            trigger_labels.push(TriggerLabel {
                position: Position { x: transform[12], y: transform[13], z: transform[14] },
                category: *category,
                name: name_bytes(&trigger.name),
            });
            let outline = trigger::outline(shape, &transform);
            trigger_lines.extend(outline.into_iter().map(|(a, b)| Line { a, b, category: *category }));
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
        let triangles = if show_meshes {
            self.collision_meshes.triangles_near(self.handle, &position, coarse_radius, detailed_radius, MAX_TRIANGLES)
        } else {
            vec![]
        };
        let triangle_milliseconds = started.elapsed().as_secs_f32() * 1000.0;

        let camera_view = self.camera_view.to_array();
        let camera_projection = projection_from_lens(&self.camera_lens.data);
        let camera_view_address = self.resolve(&self.camera_view_offsets)?;
        let camera_lens_address = self.resolve(&self.camera_lens_offsets)?;
        self.ui_memory_manager.get_mut().update(|val| {
            val.position = position;
            val.camera_view = camera_view;
            val.camera_projection = camera_projection;
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
            val.triangle_count = fill(&mut val.triangles, &triangles);
            val.triangle_milliseconds = triangle_milliseconds;
        });
        Ok(())
    }
    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>> {
        match action {
            Action::ToggleUI {} => {
                self.active = !self.active;

                if self.active {
                    self.ui_memory_manager.get_mut().update(|val| val.should_exit = false);

                    println!("Injecting overlay dll");
                    let dll_path = env::current_exe().unwrap().with_file_name("tomb_helper_ui.dll");
                    Process::by_pid(&self.pid)
                        .unwrap()
                        .inject(dll_path)
                        .unwrap();
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
            _ => (),
        }
        Ok(())
    }
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
