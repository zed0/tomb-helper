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
use std::convert::TryInto;
use std::{env, fmt, io, time};
use std::error::Error;
use hudhook::inject::Process;
use process_memory::{Architecture, CopyAddress, DataMember, Memory, Pid, ProcessHandle};
use crate::ui_ipc_data::{projection_from_lens, BoxKind, ObjectBox, SharedMemoryManager, MAX_ATTACHED, MAX_OBJECTS, MAX_TRIANGLES, OBJECT_NAME_LENGTH};

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
    object_list: Option<ObjectList>,
    objects: Vec<(Instance, BoxKind)>,
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
            object_list: None,
            objects: vec![],
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

    fn refresh_objects(&mut self) -> Result<(), Box<dyn Error>> {
        if self.object_list.is_none() {
            self.object_list = Some(ObjectList::new(self.resolve(&self.instance_array_offsets)?));
        }

        let filters: Vec<String> = self.config.object_filters.iter().map(|f| f.to_lowercase()).collect();
        let lara = self.position.to_position();
        let distance = |p: &Position| {
            ((p.x - lara.x).powi(2) + (p.y - lara.y).powi(2) + (p.z - lara.z).powi(2)).sqrt()
        };

        let mut objects: Vec<Instance> = self.object_list.as_mut().unwrap()
            .instances(self.handle)?
            .into_iter()
            .filter(|i| distance(&i.position) <= self.config.object_radius)
            .filter(|i| {
                let name = i.name.to_lowercase();
                let full_name = i.full_name.to_lowercase();
                filters.iter().any(|f| name.contains(f.as_str()) || full_name.contains(f.as_str()))
            })
            .collect();
        objects.sort_by(|a, b| distance(&a.position).total_cmp(&distance(&b.position)));

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
                ));
            } else {
                self.lara = Some((address, shapes));
            }
        }
        boxes.extend(objects.into_iter().map(|o| (o, BoxKind::Model)));
        boxes.truncate(MAX_OBJECTS);

        // Models can change (e.g. Lara's outfit), so these are refreshed with the list
        for (object, kind) in &mut boxes {
            object.bounds = object_list::read_bounds(self.handle, object.address);
            if object.bounds.is_none() && *kind == BoxKind::Model {
                *kind = BoxKind::Estimated;
            }
        }
        self.objects = boxes;
        Ok(())
    }
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

        if self.last_object_refresh.map_or(true, |t| t.elapsed() > OBJECT_REFRESH_INTERVAL) {
            self.last_object_refresh = Some(time::Instant::now());
            self.refresh_objects()?;
            if let (true, Some(offsets)) = (self.config.collision_meshes, &self.collision_scene_offsets) {
                self.collision_meshes.refresh(self.handle, self.resolve(offsets)?)?;
            }
        }

        let probes = match (self.config.lara_probes, &self.player_probes_offsets) {
            (true, Some(offsets)) => collision::read_probes(self.handle, self.resolve(offsets)?)?,
            _ => vec![],
        };
        let proxies = match (self.config.lara_proxy, &self.player_instance_offsets, &self.proxy_type_offsets) {
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
        for (object, kind) in &self.objects {
            // Objects can be unloaded between refreshes
            let transform = match object_list::read_transform(self.handle, object.address) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let mut name = [0u8; OBJECT_NAME_LENGTH];
            let length = object.name.len().min(OBJECT_NAME_LENGTH - 1);
            name[..length].copy_from_slice(&object.name.as_bytes()[..length]);
            let (min, max) = object.bounds.unwrap_or((
                Position { x: -size, y: -size, z: -size },
                Position { x: size, y: size, z: size },
            ));
            objects.push(ObjectBox {
                transform,
                min,
                max,
                kind: *kind,
                name,
            });
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

        let capsules = match &self.lara {
            Some((address, shapes)) => collision::place_capsules(self.handle, *address, shapes)?,
            None => vec![],
        };

        let position = self.position.to_position();
        let started = time::Instant::now();
        let triangles = if self.config.collision_meshes {
            self.collision_meshes.triangles_near(self.handle, &position, self.config.collision_mesh_radius, MAX_TRIANGLES)
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
