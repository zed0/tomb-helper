# tomb-helper — project index

Speedrun helper for the Tomb Raider reboot trilogy (TR2013, ROTTR, SOTTR). Windows-only Rust tool that
attaches to the running game, reads/writes its memory via pointer chains, and reacts to global hotkeys.
User docs (hotkeys, config, cutscene timing generation) live in `README.md`.

## Build

- Single binary `tomb-helper` (`src/main.rs` → `tomb-helper.exe`): finds the game, runs handlers, listens to hotkeys.
- CI: `.github/workflows/rust.yml` (build + test on push/PR to master), `main.yml` (release on `v*` tag,
  uploads only `tomb-helper.exe`). Versioning via `cargo-release`.
- `.cargo/config.toml` holds the `crt-static` rustflags (static CRT so the exe runs standalone).
  It must stay under `.cargo/` — cargo ignores a `config.toml` at the repo root.

## Runtime flow (`src/main.rs`)

1. `main()` hands off to `derive::run` if given any CLI args; otherwise loads config, prints hotkeys, then polls every 100ms with `find_process::find_process(...)`.
2. On a match, `connect()` builds a `Vec<Box<dyn Handler>>`:
   - `CutsceneTimingGeneratorHandler` **or** `CutsceneHandler` (chosen by `config.record_cutscene_timing`)
   - `PositionHandler::new_position_handler` (Lara position) and `::new_look_at_handler` (ROTTR look-at)
   - Each constructor returns `None` if the game version lacks a required `AddressType`, so features
     silently disappear per-version.
3. Registers hotkeys with `livesplit_hotkey::Hook`; keys are grouped so one key can trigger several
   `Action`s, sent over an mpsc channel.
4. Busy loop: dispatch any received `Action` to every handler's `handle_action`, then call `handle_tick`
   on all. Exits on game process exit (`is_process_running`) or SIGINT. Errors are just `eprintln!`ed.

## File index

| File | What's there |
|---|---|
| `src/main.rs` | Entry, process polling loop, handler construction, hotkey wiring, action/tick loop |
| `src/config.rs` | `Config` (serde), loaded from `tomb-helper.json` next to the exe; default hotkeys, default gist URLs, `CutsceneTiming::On/Off` |
| `src/action.rs` | `Action` enum — every hotkey-bindable action. Movement actions carry `distance` (default 100.0) |
| `src/handler.rs` | `Handler` trait: `handle_tick`, `handle_action` |
| `src/process_details.rs` | **Per-game-version memory offsets.** `known_process_details()` table (optionally `.with_witnesses(...)` from `watch`, used only by `derive`), `AddressType` enum, `VersionIdentifier` (version string + optional PE image size to tell Steam/Epic apart) |
| `src/derive.rs` | CLI dispatch (`dump`/`derive`/`chains`/`probe`/`trace`/`watch`). `derive`: fuzzy-matches code references between builds to find static addresses, then aligns `code_trace` accesses at matched sites to map each chain level (`code`/`witness`/`shifted`/`copied`) |
| `src/code_trace.rs` | `Tracer`: data-flow trace from an instruction using a static address, following pointer loads, branches, direct calls (args + returned `rax`) and, with a live `CallResolver`, virtual calls; produces `Access` chains in table form |
| `src/watch.rs` | `watch`: attaches as a debugger (`DebugActiveProcess`), sets DR0-3 data breakpoints on up to 4 values, records accessing instructions ("witnesses"), clears and detaches. 64-bit only. Hidden `watch-raw <pid> <addr>` for testing the debugger on any process |
| `src/witness.rs` | Matches witness instructions into a new build by byte-window similarity around the displacement; the displacement change gives the new last offset |
| `src/chains.rs` | Live: `chains` traces every static reference, evaluates the chains in the running game and lists code chains landing on each current value; `probe` prints current values |
| `src/pe_image.rs` | `PeImage`: load an exe in memory layout (or a dump), or dump a running process's main module (works on Denuvo builds) |
| `src/find_process.rs` | Win32 process/module lookup (`get_pid`, `get_base_address`, `is_process_running`), version detection (image size first, else reads version string from memory), `force_version` override |
| `src/tracked_memory.rs` | `TrackedMemory<T>`: cached value + pointer-chain offsets; `fetch_from_game` / `apply_to_game`. First offset is relative to module base |
| `src/tracked_position.rs` | `TrackedPosition`: three `TrackedMemory<f32>` (x/y/z), `to_position()` |
| `src/position.rs` | Plain `#[repr(C)] Position {x,y,z}` |
| `src/position_handler.rs` | Store/restore position, toggle "active" (freezes position by re-writing it every tick), camera-relative WASD/up/down movement using `CameraSin`/`CameraCos` |
| `src/cutscene_handler.rs` | Cutscene skipping (SOTTR): checks prompt/status/timeline vs. blacklist delay, sets status=5 to skip, tracks RTA/IGT saved using timing info. Also `Blacklist` type |
| `src/cutscene_timing_info.rs` | `TimingInfo` / `TimingEntry` JSON format (cutscene ids → durations, skippable-at times) |
| `src/cutscene_timing_generator_handler.rs` | Records cutscene timings by watching memory + querying LiveSplit Server over TCP (`getcurrenttime`); writes the timing file after each cutscene |
| `src/readable_from_path.rs` | `ReadableFromPath` trait: load JSON from a local path or URL (blocking reqwest), default on failure |

## Common changes

- **Support a new game version:** run `tomb-helper derive <known exe> running` to get most of the entry, then add a `ProcessDetails::new(...)` entry in `known_process_details()`
  (`src/process_details.rs`). Use `image_size: Some(..)` when the version string is shared between
  stores. Only include the `AddressType`s you've found — handlers needing missing ones won't load.
- **Add a new memory value:** add an `AddressType` variant, add offsets per version, read it with
  `TrackedMemory::new(default, offsets.clone(), arch, base_addr)`.
- **Add a hotkey action:** add a variant to `Action` (`src/action.rs`), handle it in the relevant
  handler's `handle_action`, optionally add a default in `default_hotkeys()` (`src/config.rs`), and
  document it in `README.md`.
- **Add a new feature module:** implement `Handler`, add `mod` in `main.rs`, construct it in `connect()`.

## Gotchas

- Windows-only; non-Windows stubs in `find_process.rs` just panic.
- `PositionHandler` "active" mode writes the cached position every tick, which is what lets Lara be
  moved/frozen — movement actions are no-ops while inactive.
- `CutsceneHandler::skip` deliberately returns `Ok` when not in a cutscene (Space is also jump).
  A blacklist `skip_delay` of `20000.0` means "never skip".
- `PositionHandler` and `CutsceneHandler` both bind Space by default (`Up` and `SkipCutscene`).
- The games pause when alt-tabbed, so live tools that need activity (`watch`) take `--delay`.
