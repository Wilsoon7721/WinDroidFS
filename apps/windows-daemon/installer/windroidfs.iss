; WinDroidFS installer (Inno Setup 6)
; -----------------------------------
; Builds a single setup.exe that:
;   - installs the executables into Program Files,
;   - registers the privileged driver service (runs as SYSTEM, starts at boot),
;   - starts the tray app (windroidfs.exe), which adds itself to "Start with Windows" on its
;     first run,
;   - restores drivers and removes the service on uninstall.
;
; Build it with:    iscc installer\windroidfs.iss
; (Install Inno Setup 6 first: https://jrsoftware.org/isdl.php)
;
; This script expects the release binaries to exist already:
;   cargo build --release
; which produces target\release\windroidfs.exe, windows-daemon.exe and windroidfs-service.exe.

#define AppName "WinDroidFS"
#define AppVersion "0.1.0"
#define AppPublisher "Wilson Oon"
#define ServiceExe "windroidfs-service.exe"
#define DaemonExe "windows-daemon.exe"
#define TrayExe "windroidfs.exe"

[Setup]
; A real GUID, generated once. Windows identifies the installed product by this, so upgrades
; find the previous version and uninstall cleans it up — never change it after a release.
AppId={{E223633B-B05C-4B95-8D9B-452E3B34B740}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
UninstallDisplayName={#AppName}
; Driver-service install/remove needs Administrator.
PrivilegesRequired=admin
ArchitecturesInstallIn64BitMode=x64compatible
; Icon for setup.exe itself and for the Apps & features entry.
SetupIconFile=..\assets\windroidfs.ico
UninstallDisplayIcon={app}\{#TrayExe}
OutputDir=dist
OutputBaseFilename=WinDroidFS-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Files]
; Paths are relative to this .iss file (installer\), so step up to the crate root.
Source: "..\target\release\{#ServiceExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\{#DaemonExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\{#TrayExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; AppUserModelID is what lets the app show notifications: Windows only shows toasts from an
; app a Start menu shortcut identifies. Must match APP_ID in src/bin/windroidfs/notify.rs.
Name: "{group}\{#AppName}"; Filename: "{app}\{#TrayExe}"; AppUserModelID: "WilsonOon.WinDroidFS"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"

[Run]
; After copying files, register and start the service. The service exe does the actual
; ServiceManager work (see its `install` command), so the installer just invokes it.
Filename: "{app}\{#ServiceExe}"; Parameters: "install"; Flags: runhidden waituntilterminated; StatusMsg: "Registering the WinDroidFS driver service..."
; Start the tray app as the user who ran setup, not as the elevated installer: it mounts drives
; and writes settings for that user.
Filename: "{app}\{#TrayExe}"; Description: "Start WinDroidFS now"; Flags: postinstall nowait runasoriginaluser

[UninstallRun]
; Close the tray app first, so its files can be deleted. Ending the process also ends its
; WinFsp mount: WinFsp removes a drive whose process has gone.
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im {#TrayExe}"; Flags: runhidden waituntilterminated; RunOnceId: "CloseWinDroidFSTray"
; Then, before files are deleted: restore any swapped drivers and remove the service.
; `uninstall` restores drivers first, then deletes the service (see the service exe).
Filename: "{app}\{#ServiceExe}"; Parameters: "uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveWinDroidFSService"

[UninstallDelete]
; The driver-swap journal lives outside {app}, so it would otherwise be left behind.
Type: filesandordirs; Name: "{commonappdata}\{#AppName}"

[Code]
// Stop the service before copying files over it.
//
// Without this, reinstalling or upgrading fails: the running windroidfs-service.exe is locked
// by Windows and [Files] cannot replace it. Inno has no service support of its own, so shell
// out. On a first install there is no service and this fails harmlessly.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  // The tray app locks its exe the same way; postinstall starts it again.
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/f /im {#TrayExe}', '',
       SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\sc.exe'), 'stop WinDroidFSDriver', '',
       SW_HIDE, ewWaitUntilTerminated, ResultCode);
  // Give Windows a moment to release the executable before [Files] runs.
  Sleep(1500);
  Result := '';
end;

// The tray app's "Start with Windows" entry belongs to the user, under HKCU, so the uninstaller
// has to remove it itself. (Its settings in %APPDATA% are left, as is usual for user data.)
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#AppName}');
end;
