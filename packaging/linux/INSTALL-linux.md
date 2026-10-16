# lghub-lite on Linux (experimental)

The Linux build is new and **has not been tested on real hardware yet**. It is built and
unit-tested automatically, but whether it talks to your mouse or headset is exactly what we
need to find out. Please report what works and what does not in a
[device report](https://github.com/Skyttehehe/lghub-lite/issues/new?template=device-report.md).

## 1. Allow access to the devices (once)

Linux lets only root open `/dev/hidraw*` by default. The included udev rule gives the
logged-in user access to Logitech devices:

```sh
sudo cp 70-lghub-lite.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
```

Then unplug and replug the receiver or headset. If you already use Solaar, its rule does the
same and this step can be skipped.

## 2. Try the command-line tool

```sh
./lghub-lite devices
./lghub-lite battery c54d:1
```

`<dev>` is what `devices` prints. If you get "permission denied", step 1 has not taken effect
yet (replug the device, or log out and in).

If something does not work, run `./lghub-lite report > report.txt` and attach `report.txt`
to a device report: it holds everything needed to find the problem.

## 3. Install (optional)

```sh
mkdir -p ~/.local/bin ~/.local/share/applications
cp lghub-lite lghub-lite-bg lghub-lite-gui ~/.local/bin/
cp lghub-lite.desktop ~/.local/share/applications/
cp -r icons ~/.local/share/
```

The app then appears in your application menu as **lghub-lite**. Its tray icon uses the
StatusNotifierItem standard: KDE, XFCE, Cinnamon, MATE and LXQt show it directly; GNOME needs
the "AppIndicator and KStatusNotifierItem Support" extension. Without a tray, closing the
window quits the app.

**Tray** in the app, or `lghub-lite autostart on`, starts it at login through
`~/.config/autostart/lghub-lite.desktop`. Settings, presets, skins and battery recordings
live in `~/.config/lghub-lite/`.

## Known gaps

- Receivers: the kernel's `hid-logitech-dj` driver also creates a virtual device per paired
  mouse; lghub-lite skips those and talks through the receiver, as Solaar does. Untested.
- The kernel's own HID++ driver talks to these devices too; lghub-lite uses its own HID++
  software ID so replies do not get mixed up. Untested.
- Charging detection for voltage-only headsets is not implemented on any platform yet.
