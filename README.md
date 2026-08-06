# HD2 Preset Helper

**English** | [简体中文](README.zh-CN.md)

HD2 Preset Helper is a lightweight Windows utility for saving and applying loadout presets in Helldivers 2. Each preset can include four Stratagems and an optional Booster, and can be activated with a configurable global hotkey.

The program recognizes the game interface through screen capture and completes selections using standard mouse and keyboard input. It does not modify game files, inject code into the game process, read game memory, or alter network traffic.

## Download and install

1. Download the latest ZIP from [GitHub Releases](../../releases/latest).
2. Extract it to a separate writable folder.
3. Run `HD2PresetHelper.exe`.

To update, extract the new `HD2PresetHelper` folder over the existing folder.
Files in the `data` folder are preserved.

The application runs in the system tray. Right-click its tray icon and select
**Exit** to close it.

## Quick start

The default preset keys are `Ctrl+Shift+F7` through `Ctrl+Shift+F12`, mapped to
preset slots 1–6.

1. Open the loadout home screen and select all four Stratagems and, optionally,
   a Booster.
2. Hold `Ctrl+Shift` to show the preset overlay.
3. While holding the modifiers, press `F7`–`F12` to save the current loadout.
4. To apply it later, return to the loadout home screen with all four Stratagem
   slots empty and press the same key combination.

| Loadout state | Result |
| --- | --- |
| All four Stratagem slots filled | Save or overwrite the preset |
| All four Stratagem slots empty | Apply the preset |
| Partially filled | Reject the action without changing the loadout |

The Booster is optional and is saved together with the four Stratagems.

## Compatibility

Windows 10 version 1903 or later and Windows 11 are supported. The app has been
tested from 720p to 2160p in Windowed, Borderless Window, and Fullscreen modes,
including several non-16:9 resolutions and Windows HDR.

For Borderless Window or Fullscreen, match the in-game resolution to the
Windows desktop resolution. Unusual aspect ratios, custom display scaling, and
non-default HDR UI brightness have not been fully tested.

## Configuration

Keep the application in a writable folder. It uses the following runtime files:

- `data/config.toml` — hotkeys, overlay settings, and optional behavior.
- `data/presets.json` — saved presets.
- `data/app.log` — diagnostic log from the latest launch.

The default keys can be changed in `data/config.toml` if they conflict with
another application or system shortcut. Presets are normally saved through the
in-game controls and do not need to be edited manually.

## Troubleshooting

If a preset key does nothing, verify that the app is running in the tray, the
stable loadout home screen is open, all four Stratagem slots are filled or
empty, Helldivers 2 is in the foreground, and no other application uses the
same shortcut.

For bug reports, include `data/app.log`, the resolution and display mode,
Windows scaling and HDR status, and a screenshot of the affected screen.

## Building from source

Normal users should use the prebuilt ZIP from GitHub Releases. For development,
install a current stable Rust toolchain and run:

```powershell
cargo build --release --locked
```

The executable is written to `target\release\HD2PresetHelper.exe`.

## Legal

This is an unofficial third-party utility and is not affiliated with or
endorsed by Arrowhead Game Studios or Sony Interactive Entertainment.

The source code is licensed under the
[GNU General Public License version 3 or later](LICENSE).

Game-related names and visual assets are not covered by the GPL. See
[ASSETS.md](ASSETS.md) for details and rights-holder contact information.
