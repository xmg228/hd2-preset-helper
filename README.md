# HD2 Preset Helper

**English** | [简体中文](README.zh-CN.md)

HD2 Preset Helper is a Windows utility for saving and applying loadout presets in Helldivers 2. Presets can include Stratagems, a Booster, and equipment, with a preset panel and configurable hotkeys.

The program recognizes the game interface through screen capture and completes selections using standard mouse and keyboard input. It does not modify game files, inject code into the game process, read game memory, or alter network traffic.

## Download and install

1. Download the latest ZIP from [GitHub Releases](../../releases/latest).
2. Extract it to a separate writable folder.
3. Run `HD2PresetHelper.exe`.

To update, exit the app and extract the new folder over the existing one,
keeping your `data/` folder.

The app stays in the system tray when the panel is hidden. Left-click the tray
icon to reopen it, or right-click and select **Exit** to quit.

To uninstall, exit the app and delete its folder, including any saved presets.

## Quick start

1. Equip the loadout you want to save.
2. Press `Ctrl+Shift+Space` to open the panel, select a preset and click **Save preset**.
3. To apply it later, open the panel from either loadout page and choose
   **Apply all**, or use a preset shortcut (`Ctrl+Shift+F7` through `Ctrl+Shift+F12`
   for presets 1–6 by default).

Release all keys and mouse buttons after triggering a save or apply action.
Preset shortcuts **only apply**; use the panel to save.

### Preset panel

`Ctrl+Shift+Space` shows or hides the panel. Esc also hides it.

Hover to preview, click to select, and press Enter or double-click to apply.

Check **Stratagems & Booster**, **Equipment**, or both to choose what to save and
apply, including through preset shortcuts. Saving overwrites only the checked
sections; applying skips unsaved sections.

### Usage notes

- Switching between loadout pages uses the game's default `R` key.
- Saving Stratagems requires all four slots filled. Applying can replace existing
  selections.
- Equipment includes helmet, armor, cape, primary, secondary and throwable, but not
  weapon attachments or separate appearances of items with the same name.
- Equipment templates work across resolutions, but need to be re-saved if the game
  language changes.
- The first equipment search may take longer while the list order is learned.
  Later searches reuse the cache.

## Compatibility

Windows 10 version 1903 or later and Windows 11 are supported. The app has been
tested from 720p to 2160p in Windowed, Borderless Window, and Fullscreen modes,
including several non-16:9 resolutions and Windows HDR.

For Borderless Window or Fullscreen, match the in-game resolution to the
Windows desktop resolution. Unusual aspect ratios, custom display scaling, and
non-default HDR UI brightness have not been fully tested.

Exclusive fullscreen may minimize when the panel takes focus. Use Borderless Window
if this occurs.

## Configuration

The interface follows your system language. Open **Settings** to change the language,
shortcuts, and other preferences.

The panel shortcut works globally. Preset shortcuts support single keys and work
while the game or panel is in the foreground.

Settings and presets are stored in `data/` beside the executable. Back up this
folder to keep them.

## Troubleshooting

If a shortcut does not work, check for conflicts in **Settings → Shortcuts**.

For bug reports, include `data/app.log`, the resolution and display mode,
Windows scaling and HDR status, and a screenshot of the affected screen.

## Building from source

With Rust installed, run:

```powershell
cargo build --release --locked
```

The executable is written to `target\release\HD2PresetHelper.exe`.

## Legal

This is an unofficial third-party utility and is not affiliated with or
endorsed by Arrowhead Game Studios or Sony Interactive Entertainment.

The source code is licensed under the
[GNU General Public License version 3 or later](LICENSE).
