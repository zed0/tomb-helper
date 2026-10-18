use crate::collision::{self, CapsuleShape};
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
use crate::ui_ipc_data::{BoxKind, ObjectBox, SharedMemoryManager, MAX_CAPSULES, MAX_OBJECTS, MAX_PROBES, MAX_PROXIES, OBJECT_NAME_LENGTH};

// How often to rescan the instance array for objects matching the filters
const OBJECT_REFRESH_INTERVAL: time::Duration = time::Duration::from_secs(1);

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
    instance_array_offsets: Vec<usize>,
    player_instance_offsets: Option<Vec<usize>>,
    player_probes_offsets: Option<Vec<usize>>,
    proxy_type_offsets: Option<Vec<usize>>,
    object_list: Option<ObjectList>,
    objects: Vec<(Instance, BoxKind)>,
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
        let manager = ui_memory_manager.get_mut();
        let mut val = manager.current();
        val.debug = true;
        val.should_exit = false;
        val.object_count = 0;
        manager.set_next(val);

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
            instance_array_offsets: address_offsets.get(&AddressType::InstanceArray)?.clone(),
            player_instance_offsets: address_offsets.get(&AddressType::PlayerInstance).cloned(),
            player_probes_offsets: address_offsets.get(&AddressType::PlayerProbes).cloned(),
            proxy_type_offsets: address_offsets.get(&AddressType::ProxyComponentType).cloned(),
            object_list: None,
            objects: vec![],
            lara: None,
            last_object_refresh: None,
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

// The game's projection matrix is reused for another pass with a narrower fov part way through each
// frame, so build the main camera's from its lens instead: near, far, near, far, fov, aspect, fov, aspect.
// The second copies are the more stable ones.
fn projection_from_lens(lens: &[f32; 8]) -> [f32; 16] {
    let (near, far, fov, aspect) = (lens[2], lens[3], lens[6], lens[7]);
    let y = 1.0 / (fov / 2.0).tan();
    let z = far / (far - near);
    // Row-major for row vectors, like the game's
    [
        y / aspect, 0.0, 0.0, 0.0,
        0.0, y, 0.0, 0.0,
        0.0, 0.0, z, 1.0,
        0.0, 0.0, -near * z, 0.0,
    ]
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
        }

        let probes = match (self.config.lara_probes, &self.player_probes_offsets) {
            (true, Some(offsets)) => collision::read_probes(self.handle, self.resolve(offsets)?)?,
            _ => vec![],
        };
        let proxies = match (self.config.lara_proxy, &self.player_instance_offsets, &self.proxy_type_offsets) {
            (true, Some(instance), Some(type_id)) => {
                collision::read_proxy(self.handle, self.resolve(instance)?, self.resolve(type_id)?)?
            }
            _ => vec![],
        };

        let manager = self.ui_memory_manager.get_mut();
        let mut val = manager.current();
        val.position = self.position.to_position();
        val.camera_view = self.camera_view.to_array();
        val.camera_projection = projection_from_lens(&self.camera_lens.data);

        let size = self.config.object_box_size;
        let mut count = 0;
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
            val.objects[count] = ObjectBox {
                transform,
                min,
                max,
                kind: *kind,
                name,
            };
            count += 1;
        }
        val.object_count = count;

        for (slot, probe) in val.probes.iter_mut().zip(probes.iter().take(MAX_PROBES)) {
            *slot = *probe;
        }
        val.probe_count = probes.len().min(MAX_PROBES);
        for (slot, proxy) in val.proxies.iter_mut().zip(proxies.iter().take(MAX_PROXIES)) {
            *slot = *proxy;
        }
        val.proxy_count = proxies.len().min(MAX_PROXIES);

        val.capsule_count = 0;
        if let Some((address, shapes)) = &self.lara {
            let capsules = collision::place_capsules(self.handle, *address, shapes)?;
            for (slot, capsule) in val.capsules.iter_mut().zip(capsules.iter().take(MAX_CAPSULES)) {
                *slot = *capsule;
            }
            val.capsule_count = capsules.len().min(MAX_CAPSULES);
        }
        manager.set_next(val);
        Ok(())
    }
    fn handle_action(&mut self, action: Action) -> Result<(), Box<dyn Error>> {
        match action {
            Action::ToggleUI {} => {
                self.active = !self.active;

                if self.active {
                    let manager = self.ui_memory_manager.get_mut();
                    let mut val = manager.current();
                    val.should_exit = false;
                    manager.set_next(val);

                    println!("Injecting overlay dll");
                    let dll_path = env::current_exe().unwrap().with_file_name("tomb_helper_ui.dll");
                    Process::by_pid(&self.pid)
                        .unwrap()
                        .inject(dll_path)
                        .unwrap();
                } else {
                    println!("Ejecting injected overlay dll");
                    let manager = self.ui_memory_manager.get_mut();
                    let mut val = manager.current();
                    val.should_exit = true;
                    manager.set_next(val);
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
        let manager = self.ui_memory_manager.get_mut();
        let mut val = manager.current();
        val.should_exit = true;
        manager.set_next(val);
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
