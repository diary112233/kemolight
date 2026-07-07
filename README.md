# KemoLight

KemoLight is a lightweight egui brightness controller. The first implementation targets Ubuntu GNOME and other Linux desktops with StatusNotifierItem/AppIndicator support.

## Ubuntu GNOME Support

- Main window: egui/eframe native window.
- Top bar/tray integration: freedesktop StatusNotifierItem through the Rust `ksni` crate.
- Internal laptop panels: `/sys/class/backlight`.
- External monitors: `ddcutil` using DDC/CI MCCS VCP code `0x10`.
- If tray registration fails, the app keeps running in normal window mode.
- Hide to Tray hides the eframe window and keeps the tray item alive; Quit exits the process.
- On Linux with tray support, the app prefers X11/XWayland for the main window because Wayland does not support hiding and later re-showing an existing window through winit.
- Tray menu contents are refreshed through StatusNotifierItem/DBusMenu update events after brightness state changes.

GNOME usually needs AppIndicator support enabled for tray/top-bar icons:

```bash
sudo apt update
sudo apt install gnome-shell-extension-appindicator
```

## Linux Dependencies

Install the desktop and monitor-control dependencies:

```bash
sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev \
  libxkbcommon-x11-dev libasound2-dev libfontconfig1-dev ddcutil i2c-tools
```

External monitors must have DDC/CI enabled in the monitor OSD. Some monitors ship with DDC/CI disabled.

For DDC/CI access without root, Ubuntu commonly needs `i2c-dev` and group permissions:

```bash
sudo modprobe i2c-dev
sudo usermod -aG i2c "$USER"
```

Log out and back in after changing groups. If `ddcutil detect` reports permission errors, check `/dev/i2c-*` permissions and group membership.

## Run

```bash
cargo run
```

Useful debugging commands:

```bash
ddcutil detect
ddcutil getvcp 10
ls -la /sys/class/backlight
RUST_BACKTRACE=1 cargo run
```

## Configuration

Config is read from:

```text
~/.config/kemolight/config.toml
```

Example:

```toml
start_hidden = false
brightness_step = 5
preferred_backend = "auto" # auto, ddcutil, sysfs, mock
```

## Features

- `tray`: enables Linux StatusNotifierItem tray integration.
- `linux-ddcutil`: enables external monitor control through `ddcutil`.
- `mock-backend`: includes a mock backend for development and tests.

Default features enable tray, linux-ddcutil, and mock-backend. eframe provides Wayland and X11 support through its default native features; the app chooses X11/XWayland when tray hiding is available.

## Current TODOs

- Windows and macOS backends are skeletons.
- GNOME native shell integration still depends on the desktop having an SNI/AppIndicator host.
- Sysfs writes may require distro-specific policy or helper integration for non-root users.
- DDC/CI behavior varies by monitor, cable, dock, GPU driver, and permissions.
