# windows-daemon

The Windows half of WinDroidFS. Three executables plus a shared library:

| Binary | Runs as | Job |
| --- | --- | --- |
| `windroidfs` | normal user, in the tray | Everyday use: mounts phones as they're plugged in, in the background. |
| `windows-daemon` | normal user, in a terminal | The same connection code, printing what it does. For development. |
| `windroidfs-service` | SYSTEM (Windows service) | The only process that changes USB drivers. |

Only one of `windroidfs` and `windows-daemon` can run at a time (`instance.rs`): two would race
for the phone. The user-level ones talk to the service over a named pipe (`\\.\pipe\WinDroidFS.driver`) whose ACL admits only
Administrators and SYSTEM. This is the usual Windows shape for an app that needs privileged
hardware access: the user-facing part stays unprivileged, so there is no UAC prompt per session.

## Layout

```
src/
  lib.rs                      shared library (windroidfs) + the android USB ids
  session.rs                  one connection: driver swap, handshake, ping, mount, wait
  frame.rs                    framing for messages on the USB link
  phone.rs                    the USB thread + a typed client for phone requests
  winfsp_fs.rs                the WinFsp mount (the phone as a drive letter)
  webdav.rs                   the WebDAV fallback mount
  instance.rs                 one copy at a time
  ipc.rs                      daemon <-> service pipe protocol + transport
  driver/
    mod.rs                    swap / rollback / recovery, and the crash journal
    setupapi.rs               Windows device-installation API calls
    package.rs                creating and signing WinUSB driver packages
  main.rs                     windows-daemon: the console front end
  bin/windroidfs/             windroidfs: the tray front end
    main.rs                   supervisor thread + tray icon and menu
    settings.rs               %APPDATA%\WinDroidFS\settings.ini
    autostart.rs              "Start with Windows" (HKCU\...\Run)
  bin/windroidfs-service.rs   the privileged service
installer/windroidfs.iss      Inno Setup script
```

The `unsafe` Windows calls are confined to a few files, by subject: `setupapi.rs` (devices),
`package.rs` (driver packages), `ipc.rs` (the pipe), `winfsp_fs.rs` (loading WinFsp), and the
tray app's `autostart.rs` (registry) and message loop.

## The tray app

`windroidfs.exe` is what the installer starts and what runs at sign-in. It has no window: a
tray icon whose tooltip and menu say what's happening ("Wilson's S25+ is mounted at Z:"). Left
click opens the drive. The menu has:

- **Mount phones when plugged in**: the on/off switch. Off leaves phones alone, so MTP works.
- **Drive letter**: automatic (first free from Z: down), or a fixed letter. A taken letter
  falls back to automatic. Changing it while mounted remounts straight away.
- **Options**: open in Explorer when mounted; read-only; hide dot-files.
- **Troubleshooting**: use WebDAV instead of WinFsp; keep a log; open the log folder.
- **Start with Windows**: on by default after the first run of a release build.

Settings live in `%APPDATA%\WinDroidFS\settings.ini`, one `key=value` per line, every one of
them also in the menu. (Except "Start with Windows", which is the registry's `Run` entry, so
Task Manager's Startup tab and the menu can't disagree.) A setting that only applies to a new
mount (read-only, dot-files, WebDAV) remounts a mounted phone at once.

**Read-only** is enforced by `PhoneFs` itself ("The media is write protected"). WinFsp's own
read-only flag only *reports* the volume as read-only; it doesn't stop writes reaching us.

**Logging.** The connection code reports through the `log` crate (`logging.rs`): to the console
for `windows-daemon` and debug builds of the tray app, and to
`%LOCALAPPDATA%\WinDroidFS\windroidfs.log` when "Keep a log" is on (the default). The file is
capped at 1 MiB; the previous one is kept as `windroidfs.old.log`.

**Noticing a phone.** The supervisor doesn't poll. `usb_watch.rs` registers with Windows'
Configuration Manager (`CM_Register_Notification`) for every USB device arriving or leaving,
and the supervisor sleeps until one does (or for 30s at most, as a fallback). Then it checks
whether what's plugged in is an Android phone. No window or message loop is involved. (AutoPlay
is unrelated: it's the shell offering to open new drives and media.)

When a phone appears, the supervisor runs a connection.
When a connection ends by itself (the phone was lost, or connecting failed) it waits for the
phone to be **unplugged** before trying again; otherwise a phone left plugged in after tapping
Disconnect on it would be retried forever. Replugging, or switching off and on, retries.

Once the phone is in Accessory Mode, the PC waits as long as it stays plugged in for the phone
*app* to answer ("Open WinDroidFS on your phone to finish connecting"), pinging every half
second; it mounts the moment the app picks up the link. The app, for its part, looks for the
accessory every second while it's on screen rather than relying only on Android's
"accessory attached" announcement, which a replug while the app was open didn't always deliver.

**Link speed.** The PC reads the speed the USB link was negotiated at (libusb's
`Device::speed`) and sends it in every ping, since the phone can't see it. The tray status says
"(USB 2.0)", and a link slower than USB 3 gets one toast notification ("Maxes out at 60 MB/s,
use a USB 3.0 port or USB 3.0 supported cable to increase this"); the phone's dashboard shows
the same on its "USB Type" card. Toasts need the Start menu shortcut's `AppUserModelID`, which
the installer sets (`notify.rs`); debug builds borrow PowerShell's.

The drive is named after the phone ("Wilson's S25+", from Android's device name), cleaned up by
`winfsp_fs::volume_label`: no control characters or `\ / : * ? " < > |`, at most 32 UTF-16
units cut between characters, "Android" if nothing is left.

## How the phone is mounted

**WinFsp** is the primary mount: a real drive letter, served by `winfsp_fs.rs`, readable and
writable. If WinFsp isn't installed the daemon says so, links to it, and falls back to
**WebDAV** (`webdav.rs`), which needs nothing installed but is slower, read-only, and refuses
files over ~50 MB. `--webdav` forces the fallback.

Writes go to the phone as they happen, at most 1 MiB per request. The phone keeps the file
being written open between requests, and when Windows closes it the PC sends a flush: the
phone syncs the file to storage and tells the media library, so new photos show up in the
gallery. Deleting is deferred to that same close, as Windows expects. If the link drops in the
middle of a copy, the partial file stays on the phone.

**When the phone goes away.** Unplugging gives a USB error at once; a phone that just stops
answering is caught by a ping the USB thread sends whenever the link has been idle for 3s. The
first failure marks the link dead for good, so every later request fails instantly instead of
each waiting out a 15s timeout (which is what froze Explorer). The daemon then unmounts, checks
for the phone every 5s, three times, and if it's back runs the whole handshake again and
remounts; otherwise it exits. (That's the console; the tray app just goes back to waiting.)
On the phone, the link runs in a foreground service
(`LinkService.kt`), so closing the app doesn't end it. Its notification has a Disconnect button.

Both talk to the phone through `phone.rs`. Every message is framed
(`[magic "WD"][u32 length][payload][pad]`, see `frame.rs` and the phone's `Framing.kt`) and
carries a `request_id`, so a late reply to a timed-out request is dropped instead of being
handed to the next caller. Both framing implementations are tested against the same bytes.

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

Building needs **LLVM** as well as Rust: the WinFsp bindings are generated with bindgen,
which needs libclang. (`winget install LLVM.LLVM`.) WinFsp's own developer files are *not*
needed; the bindings crate ships them.

```bash
cargo build
cargo test
cargo clippy
```

Run the service in the foreground, from an **Administrator** terminal:

```bash
cargo run --bin windroidfs-service -- run
```

Then, in an ordinary terminal, either the console daemon or the tray app (a debug build of
the tray app keeps a console window for its log, and never adds itself to startup):

```bash
cargo run --bin windows-daemon
cargo run --bin windroidfs
```

### Useful commands

| Command | What it does |
| --- | --- |
| `windroidfs` | The tray app. |
| `windows-daemon` | Console run: handshake, then mount the phone (WinFsp, else WebDAV). |
| `windows-daemon --webdav` | Same, but always use the WebDAV mount. |
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

Live pipe tests. They create the real pipe, so they must run one at a time and not alongside a
running service. They don't need Administrator, and one of them exists to keep it that way:

```bash
cargo test live_pipe -- --ignored --test-threads=1 --nocapture
```

The WinFsp mount, end to end, with no phone: mounts a real drive letter backed by a temporary
local folder, then lists, reads and writes (including files large enough to span several
requests), appends, truncates, renames, deletes, and checks attributes and errors. Needs WinFsp
installed:

```bash
cargo test mounts_and_serves -- --ignored --nocapture
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

The icon has no background tile: an amber folder filling the canvas, with the arrow cut
*through* it, so it reads on light and dark taskbars alike. The `.ico` deliberately carries
*different artwork per size*: a double-headed transfer arrow at 32px and above, and a single
arrow at 16 and 24px, where the double head's two points merge into an unreadable lump. The
Android launcher icon matches: a transparent adaptive-icon background, and a foreground folder
sized to the 66dp circle every launcher mask keeps (`apps/android-host/.../res/drawable/`). The
generated `.webp` files only matter on API 24-25.

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
