# Tomb-helper
[![GitHub Workflow Status](https://img.shields.io/github/workflow/status/zed0/tomb-helper/Rust)](https://actions-badge.atrox.dev/zed0/tomb-helper/goto)
[![GitHub release (latest SemVer)](https://img.shields.io/github/v/release/zed0/tomb-helper?sort=semver)](https://github.com/zed0/tomb-helper/releases/latest)

A speedrun helper for the Tomb Raider reboot series

Current features:
- Save and restore positions
- Move Lara directly
- Skip cutscenes and record how much time was saved (in Shadow)
- Supports Tomb Raider 2013, Rise of the Tomb Raider, and Shadow of the Tomb Raider

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
		{"key": "Space", "action": {"SkipCutscene": {}}},
		{"key": "W", "action": {"Forward": {"distance": 100.0}}},
		{"key": "S", "action": {"Backward": {"distance": 100.0}}},
		{"key": "A", "action": {"Left": {"distance": 100.0}}},
		{"key": "D", "action": {"Right": {"distance": 100.0}}},
		{"key": "Space", "action": {"Up": {"distance": 100.0}}},
		{"key": "C", "action": {"Down": {"distance": 100.0}}}
	],
	"cutscene_blacklist_file": "https://gist.githubusercontent.com/Atorizil/734a7649471f0fa0a2a9f92a167e294b/raw/Blacklist.json",
	"cutscene_timing_file": "https://gist.githubusercontent.com/zed0/762a5790501af95189344834bc210616/raw/tomb-helper-timing-info.json",
	"record_cutscene_timing": {"Off": {}}
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
- `Forward` (can take a distance, which defaults to `100.0`)
- `Backward` (can take a distance, which defaults to `100.0`)
- `Left` (can take a distance, which defaults to `100.0`)
- `Right` (can take a distance, which defaults to `100.0`)
- `Up` (can take a distance, which defaults to `100.0`)
- `Down` (can take a distance, which defaults to `100.0`)

The available keys are listed in the [livesplit_hotkey library documentation](https://docs.rs/livesplit-hotkey/0.5.0/livesplit_hotkey/linux/enum.KeyCode.html).

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
