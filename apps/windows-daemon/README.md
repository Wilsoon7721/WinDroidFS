# windows-daemon

The Windows half of WinDroidFS. Two executables plus a shared library:

| Binary | Runs as | Job |
| --- | --- | --- |
| `windows-daemon` | normal user | Finds the phone, does the AOA handshake, talks protobuf over USB. |
| `windroidfs-service` | SYSTEM (Windows service) | The only process that changes USB drivers. |

They talk over a named pipe (`\\.\pipe\WinDroidFS.driver`) whose ACL admits only
Administrators and SYSTEM. This is the usual Windows shape for an app that needs privileged
hardware access: the user-facing part stays unprivileged, so there is no UAC prompt per session.

## Layout

```
src/
  lib.rs                      shared library (windroidfs) + the android USB ids
  ipc.rs                      pipe protocol + transport
  driver/
    mod.rs                    swap / rollback / recovery, and the crash journal
    setupapi.rs               all the unsafe Windows API calls
  main.rs                     the user-level daemon
  bin/windroidfs-service.rs   the privileged service
installer/windroidfs.iss      Inno Setup script
```

Six files. Every `unsafe` block lives in `setupapi.rs` so there is one place to look when
something misbehaves at the Windows boundary.

## Driver switching, in one paragraph

The AOA handshake must be sent to the phone's *normal* USB identity, which Windows binds to the
vendor's own driver (libusb can't use it). So the service switches that identity to WinUSB for
the few seconds the handshake takes, then puts the original driver back. The accessory identity
(`18D1:2D0x`) is left on WinUSB permanently, since it only exists while WinDroidFS is running and
MTP/ADB never use it.

**One-time setup per phone:** a WinUSB driver package for that exact VID:PID must already exist
in Windows' driver store. Running Zadig once creates it. This code only *switches between*
packages that are already installed; it never creates or signs them.

## Development

```bash
cargo build
cargo test
cargo clippy
```

Run the service in the foreground, from an **Administrator** terminal:

```bash
cargo run --bin windroidfs-service -- run
```

Then, in an ordinary terminal:

```bash
cargo run --bin windows-daemon
```

### Useful commands

| Command | What it does |
| --- | --- |
| `windows-daemon` | Normal run: handshake + protobuf ping. |
| `windows-daemon restore-drivers` | Undo every driver swap (asks the service). |
| `windroidfs-service run` | Foreground service, for development. |
| `windroidfs-service install` | Register and start the service. |
| `windroidfs-service uninstall` | Restore drivers, then remove the service. |
| `windroidfs-service start` / `stop` | Start/stop the installed service. |

### Diagnostics

Read-only: prints what the swap logic *would* decide for each Android device, changing nothing.

```bash
cargo test print_driver_plan -- --ignored --nocapture
```

Live pipe tests. These need an **Administrator** terminal, because the pipe's ACL excludes
unelevated clients:

```bash
cargo test live_pipe -- --ignored --test-threads=1 --nocapture
```

## Building the installer

Needs [Inno Setup 6](https://jrsoftware.org/isdl.php).

```bash
cargo build --release
iscc installer\windroidfs.iss
```

The result lands in `dist\`. Installing asks for Administrator once, registers the service to
start at boot, and uninstalling restores any swapped drivers before removing the service.

## The app icon

`assets/windroidfs.ico` is embedded into both `.exe` files by `build.rs`, which compiles
`assets/icon.rc` with `rc.exe` from the Windows SDK. No crate dependency is involved, and if the
SDK isn't installed the build still succeeds with a warning and no icon.

The master artwork lives at the repo root in `assets/`. To change it, edit
`assets/build_icons.py`, then:

```bash
python assets/build_icons.py windroidfs
python assets/build_icons.py --android
cp assets/windroidfs.ico apps/windows-daemon/assets/windroidfs.ico
```

The `.ico` deliberately carries *different artwork per size*: a double-headed transfer arrow at
32px and above, and a single arrow at 16 and 24px, where the double head's two points merge into
an unreadable lump. The Android side uses the adaptive-icon vectors in
`apps/android-host/.../res/drawable/`; the generated `.webp` files only matter on API 24-25.

## Things that bite (learned the hard way)

**Recording a driver is not loading it.** `DiInstallDevice` returns success, and Windows writes
the new package into the devnode's `DriverInfPath` and `SPDRP_SERVICE`, while the kernel carries
on running the old driver. Both of the obvious checks therefore lie. The honest signals are the
bound service name *and* the absence of child devnodes: WinUSB owns a whole device and creates
no children, so a surviving `…&MI_00` / `…&ADB` child means the old composite stack is still the
live one. Zadig shows the truth because it reads the running driver.

**A tie on rank goes to the vendor.** Here `oem6.inf` (Samsung) and `oem10.inf` (libwdi) both
rank `00FF0001`, and Samsung's is dated 2026 against libwdi's 2012, so Windows keeps preferring
it. `DiInstallDevice` lets Windows decide; `UpdateDriverForPlugAndPlayDevicesW` with
`INSTALLFLAG_FORCE` does not. That's what Zadig uses.

**`need_reboot` means "not applied".** The install APIs return it, and ignoring it is how a
pending change gets reported as a completed one.

**The device must actually restart.** Windows applies a driver change when the device next
starts, and a phone whose MTP, modem and ADB children are in use never stops.
`CM_Query_And_Remove_SubTreeW` + `CM_Reenumerate_DevNode` forces it without unplugging. A veto
(`CR_REMOVE_VETOED`) names the blocking *device* and a reason code — never the process, which
would need undocumented handle enumeration. The usual culprit is the ADB interface held by a
running `adb` server; `adb kill-server` frees it. Zadig has the same limitation and simply
leaves the change pending until you replug, which is why it worked only sometimes.

**Never hold a libusb `Context` or `Device` across a driver change.** The devnode is destroyed
and rebuilt, so later *using* a cached device — or merely dropping it — crashes the process with
an access violation. Build a fresh `Context` per phase.

**Pipe details.** The ACL must admit interactive users, or the unelevated daemon cannot reach
its own service (UAC makes Administrators a deny-only SID). Grant an explicit mask rather than
`GRGW`, so clients don't get `FILE_CREATE_PIPE_INSTANCE` and the right to squat the name. And do
not call `DisconnectNamedPipe` on teardown: it discards data the client has not read yet, which
silently destroys every response. Closing the handle lets the client drain it first.

## Safety nets

If a driver swap is interrupted, it is always recoverable:

- The service holds the open swap; dropping it (service stop) rolls back.
- A journal at `%ProgramData%\WinDroidFS\driver-swap.journal` is written *before* the driver is
  touched and deleted only *after* rollback succeeds, so a crash or power loss is finished on the
  next service start.
- `windows-daemon restore-drivers` puts everything back by hand.
