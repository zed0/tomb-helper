# tomb-helper — project index

Speedrun helper for the Tomb Raider reboot trilogy (TR2013, ROTTR, SOTTR). Windows-only Rust tool that
attaches to the running game, reads/writes its memory via pointer chains, and reacts to global hotkeys.
User docs (hotkeys, config, cutscene timing generation) live in `README.md`.

## Build outputs (two crates from one package — see `Cargo.toml`)

| Target | Entry | Output | Purpose |
|---|---|---|---|
| bin `tomb-helper` | `src/main.rs` | `tomb-helper.exe` | Main external process: finds game, runs handlers, listens to hotkeys |
| lib `tomb_helper_ui` (cdylib) | `src/lib.rs` | `tomb_helper_ui.dll` | ImGui overlay injected *into* the game (DX12 via `hudhook`) |

- The bin and lib share `src/ui_ipc_data.rs` and `src/position.rs` by each declaring them with `mod` —
  they're compiled into both, so `UiData` must stay `#[repr(C)]` with identical layout on both sides.
- `hudhook` (0.9.x) is patched to a local checkout at `../hudhook` (`[patch.'crates-io']`), which is
  upstream `main` plus a local `Process::by_pid()` in `src/inject.rs` (not upstream; used by
  `OverlayHandler`). Builds need that sibling directory. The patch's version must match the
  `hudhook = "..."` requirement or cargo silently ignores it and uses crates.io.
- CI: `.github/workflows/rust.yml` (build + test on push/PR to master), `main.yml` (release on `v*` tag,
  uploads only `tomb-helper.exe`). Versioning via `cargo-release`.
- `.cargo/config.toml` holds the `crt-static` rustflags (static CRT so the exe/dll run standalone).
  It must stay under `.cargo/` — cargo ignores a `config.toml` at the repo root.

## Runtime flow (`src/main.rs`)

1. `main()` hands off to `derive::run` if given any CLI args; otherwise loads config and offsets (`OffsetsSource`), prints hotkeys, then polls every 100ms with `find_process::find_process(...)`, reloading the offsets when the file changes.
2. On a match, `connect()` builds a `Vec<Box<dyn Handler>>` (`create_handlers`):
   - `CutsceneTimingGeneratorHandler` **or** `CutsceneHandler` (chosen by `config.record_cutscene_timing`)
   - `OverlayHandler`
   - `PositionHandler::new_position_handler` (Lara position) and `::new_look_at_handler` (ROTTR look-at)
   - Each constructor returns `None` if the game version lacks a required `AddressType`, so features
     silently disappear per-version.
3. Registers hotkeys with `livesplit_hotkey::Hook`; keys are grouped so one key can trigger several
   `Action`s, sent over an mpsc channel.
4. Busy loop: dispatch any received `Action` to every handler's `handle_action`, then call `handle_tick`
   on all. Exits on game process exit (`is_process_running`) or SIGINT. Errors are just `eprintln!`ed.
5. Every 500ms it checks the offsets file; if the running version's offsets changed the handlers are recreated
   (losing their state), and if the game no longer matches any version it disconnects.

## File index

| File | What's there |
|---|---|
| `src/main.rs` | Entry, process polling loop, handler construction, hotkey wiring, action/tick loop |
| `src/config.rs` | `Config` (serde), loaded from `tomb-helper.json` next to the exe; default hotkeys, default gist URLs, `CutsceneTiming::On/Off` |
| `src/action.rs` | `Action` enum — every hotkey-bindable action. Movement actions carry `distance` (default 100.0) |
| `src/handler.rs` | `Handler` trait: `handle_tick`, `handle_action` |
| `offsets.json` | **Per-game-version memory offsets** (built into the exe with `include_str!`). Hex strings, `bits`, optional `image_size` (to tell Steam/Epic apart), optional `witnesses` (from `watch`, only used by `derive`), ignored `unverified` |
| `src/offsets_file.rs` | JSON schema (`OffsetsFile`/`VersionEntry`/`Hex`, `deny_unknown_fields` to catch typos), `OffsetsSource` (`tomb-helper-offsets.json` next to the exe overrides the built in file; `changed()` for hot reload), `known_process_details()` for the CLI tools, `to_json` (one line per offset list) |
| `src/process_details.rs` | Types only: `AddressType` enum (serde names are the JSON keys), `ProcessDetails`, `VersionIdentifier` |
| `src/derive.rs` | CLI dispatch (`dump`/`derive`/`chains`/`probe`/`trace`/`watch`, hidden `watch-raw`/`disasm`; flags `--version`/`--seconds`/`--delay`/`--write`/`--watch`). `derive`: fuzzy-matches code references between builds to find static addresses, then aligns `code_trace` accesses at matched sites to map each chain level (`code`/`witness`/`shifted`/`copied`) |
| `src/code_trace.rs` | `Tracer`: data-flow trace from an instruction using a static address, following pointer loads, branches, direct calls (args + returned `rax`) and, with a live `CallResolver`, virtual calls; produces `Access` chains in table form |
| `src/watch.rs` | `watch <AddressType>...`: attaches as a debugger (`DebugActiveProcess`), sets DR0-3 data breakpoints on up to 4 values (resolved after `--delay`), records accessing instructions ("witnesses") and the caller chain of each (return addresses scanned off the stack), clears and detaches. `--write` breaks on writes only. 64-bit only. Hidden `watch-raw <pid> <addr or +rva>` for any address, and `disasm <pid> <addr or +rva> [count]` (prints RVAs; call targets are absolute) |
| `src/witness.rs` | Matches witness instructions into a new build by byte-window similarity around the displacement; the displacement change gives the new last offset |
| `src/chains.rs` | Live: `chains` traces every static reference, evaluates the chains in the running game and lists code chains landing on each current value; `probe` prints current values |
| `src/pe_image.rs` | `PeImage`: load an exe in memory layout (or a dump), or dump a running process's main module (works on Denuvo builds) |
| `src/find_process.rs` | Win32 process/module lookup (`get_pid`, `get_base_address`, `is_process_running`), version detection (image size first, else reads version string from memory), `force_version` override |
| `src/tracked_memory.rs` | `TrackedMemory<T>`: cached value + pointer-chain offsets; `fetch_from_game` / `apply_to_game`. First offset is relative to module base |
| `src/tracked_position.rs` | `TrackedPosition`: three `TrackedMemory<f32>` (x/y/z), `to_position()` |
| `src/position.rs` | Plain `#[repr(C)] Position {x,y,z}` (shared with the DLL) |
| `src/position_handler.rs` | Store/restore position, toggle "active" (freezes position by re-writing it every tick), camera-relative WASD/up/down movement using `CameraSin`/`CameraCos` |
| `src/cutscene_handler.rs` | Cutscene skipping (SOTTR): checks prompt/status/timeline vs. blacklist delay, sets status=5 to skip, tracks RTA/IGT saved using timing info. Also `Blacklist` type |
| `src/cutscene_timing_info.rs` | `TimingInfo` / `TimingEntry` JSON format (cutscene ids → durations, skippable-at times) |
| `src/cutscene_timing_generator_handler.rs` | Records cutscene timings by watching memory + querying LiveSplit Server over TCP (`getcurrenttime`); writes the timing file after each cutscene |
| `src/readable_from_path.rs` | `ReadableFromPath` trait: load JSON from a local path or URL (blocking reqwest), default on failure |
| `src/overlay_handler.rs` | On `ToggleUI` injects `tomb_helper_ui.dll` into the game (`hudhook::inject`); each tick streams into shared memory: position, `CameraView`, a projection built from `CameraLens`, object boxes (objects matching `config.overlay` filters, rescanned every second), and Lara's collision from `collision.rs` (hit capsules, ground probes, proxy capsules, with a 150ms hold on "pushing"); sets `should_exit` to eject on toggle-off/drop. `TrackedCameraMatrix` reads a 4x4 matrix in one copy |
| `src/object_list.rs` | Reads the game's static instance array (`InstanceArray`, 0x120 byte records until a zero header): position, placement names, world matrix (built from the instance's position + Rx*Ry*Rz Euler rotation, with only the scale from `cdc::TransformComponent`, which goes stale for Lara), and model bounds (`read_bounds`: component table -> DrawableComponent -> InstanceDrawable -> CommonRenderModelInstance -> CommonRenderModel -> `Mesh` header min/max). Layout offsets are constants found in ROTTR Steam |
| `src/collision.rs` | Lara's collision, see "Lara's collision" below: hit capsules (`read_capsule_shapes`/`place_capsules`), ground probes (`read_probes`), and her movement proxy with the camera/weapon proxies attached to it (`read_proxy`), including which of them is pushing her (contacts on their surface). Has unit tests for the capsule geometry |
| `src/ui_ipc_data.rs` | Shared-memory IPC (`shared_memory` crate, os_id `"TombHelperIPC-<size of UiDoubleBuffer>"` so a stale mapping from another layout is never opened): `UiData` (camera, `ObjectBox`es, `Capsule`s, `Probe`s), `UiDoubleBuffer` with a sequence counter per buffer (`set_next`; the reader uses `try_current` and keeps its last good copy, since the exe never waits), `SharedMemoryManager` (exe is owner) |
| `src/lib.rs` | Injected overlay: `TombHelperHud` implements `ImguiRenderLoop`; opens a console only if `config.overlay.console`; shows a position window and draws (near-plane clipped, in world space via `Projector`) a box + name per `ObjectBox`, Lara's capsules, probes and a circle at her feet. Colours: model boxes green, estimated yellow, collision capsules white / yellow while pushing, proxy's next shape red, probes cyan. Calls `eject()` when `should_exit` is set |

## Common changes

- **Support a new game version:** run `tomb-helper derive <known exe> running` to get most of the entry, then add it to
  `offsets.json` (test it first in `tomb-helper-offsets.json` next to the exe, which hot reloads). Use `image_size`
  when the version string is shared between stores. Only include the `AddressType`s you've found — handlers
  needing missing ones won't load.
- **Add a new memory value:** add an `AddressType` variant, add offsets per version in `offsets.json`, read it with
  `TrackedMemory::new(default, offsets.clone(), arch, base_addr)`.
- **Add a hotkey action:** add a variant to `Action` (`src/action.rs`), handle it in the relevant
  handler's `handle_action`, optionally add a default in `default_hotkeys()` (`src/config.rs`), and
  document it in `README.md`.
- **Add a new feature module:** implement `Handler`, add `mod` in `main.rs`, construct it in `connect()`.
- **Send more data to the overlay:** add a field to `UiData` (`src/ui_ipc_data.rs`), fill it in
  `OverlayHandler::handle_tick`, read it in `TombHelperHud::render` (`src/lib.rs`). Both binaries must be
  rebuilt together.

## Game memory notes (ROTTR Steam, Foundation/cdc engine)

Everything below was found in ROTTR Steam (`(Steam) v1.0 build 0.0_64`, image size 58654720), the only version with
the overlay offsets (`CameraView`, `CameraLens`, `InstanceArray`, `PlayerInstance`, `PlayerProbes`,
`ProxyComponentType`). Offsets are hex, `mod+` means relative to the exe's base. Other versions will need them found
again (`derive` plus live checks).

### Finding your way around
- MSVC RTTI is present: an object's first qword is a vtable, `vtable[-1]` is the CompleteObjectLocator whose type
  descriptor (+0xC RVA, name at +0x10) gives the class name. Enumerating every vtable in the image and scanning memory
  for them gives every live object of each class (Python + numpy over ReadProcessMemory takes seconds).
- Objects are *instances* in a static array (`InstanceArray`, mod+0x24EEAD0, 0x120 byte slots, 8267 of them, all
  with header 2, then zeros). A slot is used if +0xE0 (TransformComponent) is set. +0x10 position and +0x30 Euler
  rotation (radians, Rx*Ry*Rz) are live; +0x78 -> placement record (+0x48 short name, +0x50 full name; null for
  spawned instances like Lara); +0xD8 -> component table with a fixed slot per component type (+0x40
  DrawableComponent, +0x50 CollideComponent; the proxy's slot is the u32 at `ProxyComponentType` mod+0x1101D20).
  Components are heap objects whose +0x8 points back at the instance. Which slot Lara uses changes when she's
  reloaded, so always go through `PlayerInstance` ([0x100C6B0, 0, 0x31A8, 0] = Player+0x31A8).
- `TransformComponent` +0x30 is a world matrix, but it isn't kept up to date for everything (Lara's stays where she
  loaded in), so the overlay only takes its scale. Its +0x80 -> Lara's live bone matrices (world space, bone n of a
  shape is matrix n + 1).
- Model bounds are in each `Mesh` resource header ("Mesh" magic, +0x10 sphere centre, +0x20/+0x30 box min/max, +0x50
  sphere radius), via DrawableComponent+0x98 -> InstanceDrawable+0xC8 -> CommonRenderModelInstance+0x48 ->
  CommonRenderModel+0x50. Terrain meshes are in world space, others object space. Undrawn instances (triggers,
  markers) have no InstanceDrawable. Every base camp has one root instance whose full name contains
  `campsite_base_trx-`, at its fire.

### Camera
- `GameRender` (`[0x2E1BD08]`): +0x30 near/far, +0x40 and +0x48 fov/aspect, +0x70 camera-to-world (CameraSin/Cos are
  its first row), +0xF0 projection, +0x520 world-to-view. Matrices are row-major for row vectors (read them
  column-major into nalgebra), view space y points down the screen, world is Z up.
- +0xF0 is swapped for a narrower fov pass for ~1ms of every frame, so the overlay builds its own projection from
  +0x30 near/far and +0x48 fov/aspect (`CameraLens`); +0x48 never showed the other pass's fov.

### Lara's collision
- **Movement:** her `cdc::ProxyComponent` capsule. It doesn't reach the ground (ground probes do that) and the shape
  in use (+0x58 radius, +0x5C *full* cylinder length, +0x60 offset from her feet in her space, +0x80 tilt, +0x90
  facing, +0x30 world centre) eases over a few frames towards the shape it's changing to (+0xC0 radius/length,
  +0xD0 offset, +0xF0 tilt). The physics capsule actually swept is proxy+0x148 (body) -> +0x20 (shape: +0x0 centre,
  +0x20 rotation, +0x44 radius, +0x48 length, +0x98 type 1, +0x99 touching flag); its volume formula
  (4/3 pi r^3 + pi r^2 h) is what shows the length isn't halved.
- Shapes seen (radius, length, offset, height above her feet): standing 25/60 (0,-5,85) 30..140; aiming 25/40
  (0,-10,90) 45..135; crouched 23.7/35.8 (0,0,60); rolling 45/15 (0,-23,90) tilted ~25deg 38..142, after a 25/50
  (0,-20,92) wind up; once 25/25 (0,-25,0) tilted 90deg (unidentified). Her local -y is (probably) forward.
- **Attached proxies:** bodies are in a list (+0x30 next, +0x48 parent, +0x50 -> owning proxy+0x120). Attached to
  hers are the camera (radius 30, length 30, offset (0,-15,90), moving to (0,30,90) i.e. behind her when aiming) and
  her weapon (radius 3.5, 90 long rod in front of her at shoulder height). They only collide while she aims, which
  is why she's held further from walls then (the rod lines up with it, including at angles).
- **What's pushing her:** a shape's +0x99 is 0x2F while it's touching something: always for Lara's, always while
  aiming for the camera (it overlaps her capsule, so this is also the best "attached proxies are on" signal), and
  only against walls for the weapon. Lara's proxy's contacts (counts +0x164/+0x168, 0x60 byte records from +0x190,
  world point first, +0x10 looks like the push) include those of the attached proxies, so a contact on a capsule's
  surface means it's pushing her. Reads are often mid update, hence the overlay's 150ms hold. Lara's proxy +0x18 bit
  0x20 clears while aiming *and* rolling, so it's not an aiming flag.
- **Wall clip theory** (speedrun glitch: back to a wall, aim, crouch/roll, twist the camera): the roll swaps a radius 25
  capsule for a radius 45 one offset 23 along her local -y, and aiming makes her face the camera, so twisting it
  swings that offset capsule; one starting inside a wall is likely what lets her through. Not confirmed in code.
- **Hit capsules:** `CollideComponent`+0x70 lists 0x18 byte entries (count +0x78): +0x8 id (byte 2 is the type, 0x8A
  capsule, high u32 + 1 is the bone), +0x10 -> shape in bone space (+0x0 centre, +0x10 quaternion, +0x28 radius,
  +0x2C length). 10 `DefensiveCapsule_*` plus attack spheres. Slow to draw, off by default.
- **Ground probes:** `PlayerProbes` (mod+0x1A8C720) is a count, then 0x60 byte records from +0x70 (+0x10 hit, +0x20
  start, +0x30 end): rays per foot and one for her body. Hits far from their ray are stale.
- **How it was found:** `watch XPosition --write` -> the player move (+0x83B330, adds a delta to her position) <- 55
  callers, the main one +0x840460 which collides via +0x82B770 on the proxy; the half length's only reader is the
  easing at +0x1DD0B0, which sets the physics shape's radius/length (+0x193FD0/+0x193F80).

### Working with the live game
- The user plays; ask them to switch to the game before anything that needs it running (it pauses when alt-tabbed),
  then start with a delay. Reads work fine while it's paused.
- Throwaway exploration has been done with Python (ctypes ReadProcessMemory + numpy) from the job's tmp dir: run it
  with `-I`, which also ignores `PYTHONIOENCODING`, so use `sys.stdout.reconfigure(encoding='utf-8')`.
- Heap addresses change when the game restarts, but the proxy/body/shape objects stayed put within a session.

## Gotchas

- Windows-only; non-Windows stubs in `find_process.rs` just panic.
- `PositionHandler` "active" mode writes the cached position every tick, which is what lets Lara be
  moved/frozen — movement actions are no-ops while inactive.
- `CutsceneHandler::skip` deliberately returns `Ok` when not in a cutscene (Space is also jump).
  A blacklist `skip_delay` of `20000.0` means "never skip".
- `PositionHandler` and `CutsceneHandler` both bind Space by default (`Up` and `SkipCutscene`).
- The games pause when alt-tabbed, so live tools that need activity (`watch`) take `--delay`.
- The shared memory is backed by a file in `%TEMP%\shared_memory-rs`, which outlives a crashed or killed exe; it's
  named after the `UiData` layout's size so a changed layout gets a new one.
- A running `tomb-helper.exe` locks the build output, so `cargo build` fails until it's closed.
- In a `.claude/worktrees/` checkout the `../hudhook` patch doesn't resolve: build with
  `cargo build --config "patch.crates-io.hudhook.path='H:/hudhook'"`.
- `tomb-helper.json`'s `overlay.console` turns the injected overlay's console back on for debugging.
