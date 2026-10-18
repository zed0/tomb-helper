# Tomb-helper
[![GitHub Workflow Status](https://img.shields.io/github/actions/workflow/status/zed0/tomb-helper/rust.yml?branch=master)](https://actions-badge.atrox.dev/zed0/tomb-helper/goto)
[![GitHub release (latest SemVer)](https://img.shields.io/github/v/release/zed0/tomb-helper?sort=semver)](https://github.com/zed0/tomb-helper/releases/latest)

A speedrun helper for the Tomb Raider reboot series

Current features:
- Save and restore positions
- Move Lara directly
- Skip cutscenes and record how much time was saved (in Shadow)
- Supports Tomb Raider 2013, Rise of the Tomb Raider, and Shadow of the Tomb Raider
- Connects and disconnects from games as they are started and stopped: useful in case of crashes!

# Usage

- Download the [latest version](https://github.com/zed0/tomb-helper/releases/latest) of tomb-helper.exe
- Open a `cmd` prompt in the directory it was downloaded to (you can type `cmd<enter>` in the Windows Explorer address bar)
- Run `tomb-helper.exe`

# Configuration

Tomb-helper can be configured by a file named `tomb-helper.json` adjacent to the `tomb-helper.exe` file.

The default configuration is:
```json
{
	"hotkeys": [
		{"key": "F5", "action": {"ToggleActive": {}}},
		{"key": "F6", "action": {"StorePosition": {}}},
		{"key": "F7", "action": {"RestorePosition": {}}},
		{"key": "F8", "action": {"ResetSkipCutsceneTracker": {}}},
		{"key": "F9", "action": {"ToggleUI": {}}},
		{"key": "Space", "action": {"SkipCutscene": {}}},
		{"key": "W", "action": {"Forward": {"distance": 100.0}}},
		{"key": "S", "action": {"Backward": {"distance": 100.0}}},
		{"key": "A", "action": {"Left": {"distance": 100.0}}},
		{"key": "D", "action": {"Right": {"distance": 100.0}}},
		{"key": "Space", "action": {"Up": {"distance": 100.0}}},
		{"key": "C", "action": {"Down": {"distance": 100.0}}}
	],
	"force_version": null,
	"cutscene_blacklist_file": "https://gist.githubusercontent.com/Atorizil/734a7649471f0fa0a2a9f92a167e294b/raw/Blacklist.json",
	"cutscene_timing_file": "https://gist.githubusercontent.com/zed0/762a5790501af95189344834bc210616/raw/tomb-helper-timing-info.json",
	"record_cutscene_timing": {"Off": {}},
	"overlay": {"object_filters": ["campsite_base_trx-"], "object_radius": 5000.0, "object_box_size": 50.0, "lara_capsules": false, "lara_probes": true, "lara_proxy": true, "console": false}
}
```

## Hotkeys

The hotkeys can be customised via the `hotkeys` property.
Each entry comprises of a `key` and an `action` field.

Currently the available actions are:
- `ToggleActive`
- `StorePosition`
- `RestorePosition`
- `SkipCutscene`
- `ResetSkipCutsceneTracker` (reset the total amount of time of cutscenes skipped, prints out the previous total, suggest binding this to the same key you use to reset livesplit)
- `ToggleUI` (inject or remove the in-game overlay; experimental, DirectX 12 only)
- `Forward` (can take a distance, which defaults to `100.0`)
- `Backward` (can take a distance, which defaults to `100.0`)
- `Left` (can take a distance, which defaults to `100.0`)
- `Right` (can take a distance, which defaults to `100.0`)
- `Up` (can take a distance, which defaults to `100.0`)
- `Down` (can take a distance, which defaults to `100.0`)

The available keys are listed in the [livesplit_hotkey library documentation](https://docs.rs/livesplit-hotkey/0.5.0/livesplit_hotkey/linux/enum.KeyCode.html).

## Offsets file

The memory offsets for each supported game version are built into tomb-helper (from [`offsets.json`](./offsets.json)). To change them or add a new version without rebuilding, run

```
tomb-helper export-offsets
```

which writes them to `tomb-helper-offsets.json` next to `tomb-helper.exe`. While that file exists tomb-helper uses it instead, and reloads it whenever it changes, even while connected to a game, so you can edit offsets and test them straight away. If the file has a mistake in it the error is printed and the previous offsets are kept until it's fixed. Delete the file to go back to the built in offsets.

Each version looks like:

```json
{
  "game": "Shadow of the Tomb Raider",
  "executable": "SOTTR.exe",
  "version": "(Steam) v1.0 build 492.0_64",
  "image_size": 73662464,
  "bits": 64,
  "offsets": {
    "XPosition": ["0x14B5BD8", "0x0", "0x33E8", "0x10"],
    ...
  },
  "witnesses": {
    "XPosition": ["0x62E5A9", "0x12AFCC"],
    ...
  }
}
```

- `version` is the version string in the game's memory, read using the `Version` offsets, unless `image_size` is given, in which case the size of the game's exe in memory identifies the version instead (Steam and Epic builds share version strings)
- `offsets` are pointer chains: the first is relative to the game's exe, the rest are followed as pointers. Values a version doesn't have can be left out, and features that need them won't load
- `witnesses` are optional, see [Finding offsets for a new game version](#finding-offsets-for-a-new-game-version)
- `unverified` is ignored, and is where the tools put offsets they couldn't confirm

## Overlay

The overlay (`ToggleUI`) draws a box around game objects near Lara, labelled with the object's name.
It currently needs the `CameraView`, `CameraLens` and `InstanceArray` (and `PlayerInstance`/`PlayerProbes`/`ProxyComponentType` for Lara's collision) offsets, which only Rise of the Tomb Raider (Steam) has so far.
- `object_filters`: objects whose name contains any of these (case insensitive) get a box, `[""]` shows everything. The default, `campsite_base_trx-`, matches each base camp (at its campfire)
- `object_radius`: only objects this close to Lara are shown (the closest 64 are drawn)
- `object_box_size`: objects with a model get a green box showing its bounds, the rest (triggers, markers, ...) get a yellow box this size (half width)
- `lara_capsules`: also draw Lara's hit capsules (in magenta), which follow her pose. Off by default as it's slow
- `lara_probes`: also draw the ground probes the game casts for her feet and body (cyan lines, with a cross where they hit)
- `console`: open a console window in the game showing the overlay's logs, for debugging it
- `lara_proxy`: also draw the capsule the game moves Lara with, i.e. what she collides with walls with (white, or yellow while it's pushing her off something). When it changes shape (e.g. when she starts aiming) the shape it's changing to is drawn in red. While she aims, the proxies attached to Lara's also collide, and are drawn the same way: the camera, which moves behind her, and her weapon (a thin rod), which is what holds her further from walls in front of her

## Forcing a version

There is an experimental feature to force tomb-helper to use the memory offsets for a specific version of the game. This can be done by specifying the `force_version` key with the version you want. These are chosen from the [offsets file](#offsets-file) and must match the version in the config file **exactly**.

For example
```json
{
	"force_version": "v1.01.748.0"
}
```
would force tomb-helper to use the offsets for v1.01.748.0 even if the detected version of the game was v1.01.743.0.

## Cutscene blacklist file

The cutscene blacklist is the list of timings that are used to configure the time until cutscenes can be skipped.

The default value is [the list](https://gist.github.com/Atorizil/734a7649471f0fa0a2a9f92a167e294b) that is used by both this tool and @Atorizil's [SOTTR Cutscene Skipper](https://github.com/Atorizil/SOTTR-Cutscene-Skipper).
If you want to use a different file (e.g. to try out some new timings) you can use a pastebin or github gist. Local files may be supported at some point.

## Cutscene timing file

Similar to the cutscene blacklist file the cutscene timing file is used to calculate how much time has been saved by skipping each cutscene.
The default value is [the list](https://gist.github.com/zed0/762a5790501af95189344834bc210616) recorded by zed0, which should be sufficient for any% glitched or glitchless runs, but does not contain all the cutscenes in 100%.

### Generating a new cutscene timing file

Generating a cutscene timing file can be done using tomb-helper:

- Install the [Livesplit Server extension](https://github.com/LiveSplit/LiveSplit.Server)
- Add a Livesplit Server component to your Livesplit layout
- Ensure the game's autosplitter is enabled in Livesplit
- Ensure Livesplit is set to compare against Game Time
- Start the Livesplit Server via right clicking on Livesplit -> Control -> Start Server
- Create a tomb-helper configuration file if you don't already have one (see above)
- Change the `"record_cutscene_timing"` config entry to `{"On": {}}`
	- Optionally change the config depending on where you want the timing file output and your Livesplit Server port: `{"On": {"timing_file": "./new_cutscene_timing_file.json", "livesplit_port": 16834}}`
- Launch the game
- Launch tomb-helper
	- Check it says "Loading cutscene timing generation handler..." during the start up
- Play through the entire game, making sure to watch all the cutscenes and press E repeatedly to make sure skip prompts show up
	- Skipping cutscenes via tomb-helper will be disabled while generating the timing file
	- The recording file is saved after each cutscene, if your game crashes or you want to record in multiple sessions simply continue at a later date with the same recording file
- Save the newly created file somewhere (upload it if you want others to be able to use it)
- Change your `"cutscene_timing_file"` configuration option to the location you saved the created file
- Change your `"record_cutscene_timing"` back to `{"Off": {}}`

# Finding offsets for a new game version

tomb-helper can work out most of the memory offsets for a new version of a game from a version it already supports:

```
tomb-helper derive <from> <to> [--from-version <version>]
```

`<from>` and `<to>` are paths to the game's exe, or `running` to use the game that is currently running. Games protected by Denuvo (e.g. Shadow of the Tomb Raider v1.0 build 234.1) are encrypted on disk, so for those launch the game, wait for the main menu, and use `running`, or save a copy with `tomb-helper dump [<out.bin>]` and pass that file instead.

The version of `<from>` is detected from its image size where possible, otherwise pass `--from-version` with a version from the [offsets file](#offsets-file), e.g.

```
tomb-helper derive "C:\Games\Shadow of the Tomb Raider 492.0\SOTTR.exe" running
tomb-helper derive sottr-234.1.bin "C:\Games\Shadow of the Tomb Raider\SOTTR.exe" --from-version "v1.0 build 234.1_64"
```

It prints a version entry to add to the [offsets file](#offsets-file), with a note per offset list saying where each level came from:

- `code`: the game's code uses this offset in both versions, in matching places
- `shifted`: the code doesn't use this offset directly, but a neighbouring field moved by this much
- `copied`: no evidence, kept as it was in `<from>`

Offsets marked `UNSURE` had conflicting matches or a copied level, and are put under `unverified` for you to check. The version string needs filling in by hand. Check the result in game with `tomb-helper probe [--watch]`, which prints the current value of every offset.

Derivation works best when the offsets in `<from>` follow the same pointer chains as the game's own code. Pointer scanners tend to find arbitrary chains instead, which can't be matched up between versions. To find the chains the code uses, load into a level (and start a cutscene for the cutscene values) with a known version running, then run:

```
tomb-helper chains
```

For each value it lists the chains the game's code uses to reach the same address, and prints an entry using the best of them. Some values (e.g. most SOTTR cutscene values) are only read by code that finds the object some other way, so have no code chain and keep their current offsets.

For those, record which instructions access the value:

```
tomb-helper watch CutscenePrompt CutsceneStatus CutsceneLength --delay 15 --seconds 8
```

This attaches to the game as a debugger and puts hardware breakpoints on up to 4 values, then detaches. The game pauses when it isn't focused, so use `--delay` to give yourself time to switch back to it (and get the values in use, e.g. start a cutscene). It prints a `"witnesses"` block to add to that version's entry in the offsets file; `derive` matches those instructions in the new build to find where each field moved to (`witness` in its output).

`tomb-helper trace <exe> <static address>` prints the chains the code accesses from every reference to a static address, which helps when investigating by hand.
