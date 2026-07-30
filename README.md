# TarDrop

TarDrop is a KDE-friendly, user-local installer for portable application archives. Drop a `.tar`, `.tar.gz`, `.tgz`, `.tar.xz`, `.tar.bz2`, or `.zip` archive onto its window. It extracts the archive beneath `~/Applications`, finds a native ELF application, and writes a per-user launcher to `~/.local/share/applications`.

## Safety model

TarDrop does not use `tar`, `unzip`, a shell, or `sudo`. Archives are extracted into a private staging directory first. Every member path is checked, and absolute paths, `..`, symlinks, hard links, device files, and overwrite attempts are rejected. Only regular files and directories are accepted. It never runs archive scripts; `install.sh` is not a launch candidate. A launcher is made only for an ELF binary, and is never automatically launched.

Existing installations are either replaced, given a separate numbered directory, or cancelled. Uninstall checks that it is deleting only an immediate child of `~/Applications` and the matching XDG launcher.

## Requirements

* GNU/Linux on x86_64
* Rust stable (edition 2024 or higher)
* A working Wayland or X11 session
* A desktop opener (`xdg-open`) for the optional **Open folder** button
* `update-desktop-database` is optional; Plasma also discovers the user launcher directory directly

`eframe` is used for the GUI because it gives one native desktop build for both KDE Wayland and X11 without requiring Qt or GTK development packages. KDE uses the normal XDG desktop-entry location, so installed apps appear in Application Launcher, Kickoff, and KRunner.

## Build and run

```bash
cargo build --release
./target/release/tardrop
```

For development, use `cargo run`. Cargo downloads the Rust dependencies on the first build. On distributions with no Wayland/X11 development runtime, install the desktop graphics packages recommended by the `eframe`/winit documentation.

## Architecture

* `archive`: identifies formats and performs streaming, validated extraction.
* `security`: hashes archives, identifies ELF binaries, and validates desktop fields.
* `installer`: owns the extract → inspect → publish transaction and uninstall rules.
* `icons`: chooses a likely PNG, SVG, or XPM icon without following links.
* `desktop`: writes the per-user launcher and asks the desktop database to refresh.
* `ui`: drag-and-drop, queueing, dialogs, progress display, log, launch, and uninstall controls.
* `utils`: bounded user-directory and naming helpers.

## Notes

TarDrop intentionally rejects archives that contain symlinks, hardlinks, or special files. This is stricter than ordinary archive tools because the product’s job is safely handling untrusted downloads. Launcher discovery scores root `AppRun`, safe `Exec=` targets from nearby desktop files, conventional launcher scripts, root executables, and name matches. Dependency, documentation, and known helper-binary trees are heavily penalized. When the two top candidates are within 10 points, TarDrop shows a chooser instead of guessing.
