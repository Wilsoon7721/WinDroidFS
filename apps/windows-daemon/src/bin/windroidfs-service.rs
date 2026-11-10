//! WinDroidFS privileged driver service

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windroidfs::android::{ANDROID_VIDS, is_aoa_identity, is_normal_android_identity};
use windroidfs::driver::{self, WinUsbSwap, parse_parent_instance_id};
use windroidfs::global_drive;
use windroidfs::ipc::transport::{Connection, accept_one};
use windroidfs::ipc::{Request, Response};

const SERVICE_NAME: &str = "WinDroidFSDriver";
const SERVICE_DISPLAY: &str = "WinDroidFS Driver Service";

fn main() {
    let command = std::env::args().nth(1);
    match command.as_deref() {
        Some("run") => run_foreground(),
        Some("install") => report(manage::install()),
        Some("uninstall") => report(manage::uninstall()),
        Some("start") => report(manage::start()),
        Some("stop") => report(manage::stop()),
        Some(other) => {
            eprintln!("Unknown command {other:?}. Try: run | install | uninstall | start | stop")
        }
        // No argument: the Service Control Manager launched us.
        None => {
            if let Err(e) = service_entry::run() {
                eprintln!("Service failed to start: {e}");
            }
        }
    }
}

fn report(result: Result<String, String>) {
    match result {
        Ok(msg) => println!("{msg}"),
        Err(e) => eprintln!("❌ {e}"),
    }
}

// ===========================================================================
// The request loop (shared by the service and `run`)
// ===========================================================================

/// Holds the swap that's currently open, so BeginSwap and the later Rollback (two separate pipe
/// connections) refer to the same one. If the daemon dies before Rollback, this is dropped when
/// the loop ends, which rolls back; a hard service crash is covered by the on-disk journal.
#[derive(Default)]
struct ServiceState {
    active_swap: Option<WinUsbSwap>,
    published: Published,
}

type Published = Arc<Mutex<HashMap<char, (String, u32)>>>;

impl ServiceState {
    fn handle(&mut self, request: Request, connection: &Connection) -> Response {
        match request {
            Request::Ping => Response::Pong,

            Request::PublishDrive { letter } => match self.publish(letter, connection) {
                Ok(detail) => Response::Ok { detail },
                Err(message) => Response::Err { message },
            },

            Request::UnpublishDrive { letter } => match self.unpublish(letter, connection) {
                Ok(detail) => Response::Ok { detail },
                Err(message) => Response::Err { message },
            },

            Request::BeginSwap { instance_id } => {
                if let Err(message) = require_identity(&instance_id, Identity::Normal) {
                    return Response::Err { message };
                }
                // Finish any previous swap first, so we never hold two at once.
                self.active_swap = None;
                match WinUsbSwap::begin(&instance_id) {
                    Ok(swap) => {
                        let detail = format!("{instance_id} {}", swap.detail());
                        self.active_swap = Some(swap);
                        Response::Ok { detail }
                    }
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }

            Request::Rollback { instance_id } => {
                match self.active_swap.take() {
                    Some(swap) => match swap.rollback() {
                        Ok(outcome) => Response::Ok {
                            detail: outcome.to_string(),
                        },
                        Err(e) => Response::Err {
                            message: e.to_string(),
                        },
                    },
                    // No swap in memory (e.g. the service restarted): fall back to the journal.
                    None => match driver::recover_from_journal() {
                        Ok(Some((id, outcome))) => Response::Ok {
                            detail: format!("{id}: {outcome}"),
                        },
                        Ok(None) => Response::Ok {
                            detail: format!("no swap on record for {instance_id}"),
                        },
                        Err(e) => Response::Err {
                            message: e.to_string(),
                        },
                    },
                }
            }

            Request::EnsureWinusb { instance_id } => {
                if let Err(message) = require_identity(&instance_id, Identity::Accessory) {
                    return Response::Err { message };
                }
                match driver::ensure_winusb(&instance_id) {
                    Ok(true) => Response::Ok {
                        detail: format!("{instance_id} bound to WinUSB"),
                    },
                    Ok(false) => Response::Ok {
                        detail: format!("{instance_id} already on WinUSB"),
                    },
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }

            Request::RestoreAll => {
                match driver::restore_android_drivers(ANDROID_VIDS, is_aoa_identity) {
                    Ok(list) if list.is_empty() => Response::Ok {
                        detail: "nothing to restore".into(),
                    },
                    Ok(list) => {
                        let detail = list
                            .iter()
                            .map(|(id, o)| format!("{id}: {o}"))
                            .collect::<Vec<_>>()
                            .join("; ");
                        Response::Ok { detail }
                    }
                    Err(e) => Response::Err {
                        message: e.to_string(),
                    },
                }
            }
        }
    }
}

impl ServiceState {
    fn publish(&mut self, letter: char, connection: &Connection) -> Result<String, String> {
        sweep(&self.published);
        let target = connection
            .as_client(|| global_drive::target_of(letter))
            .map_err(|e| format!("couldn't check the caller's drives: {e}"))?
            .filter(|target| global_drive::is_volume_device(target))
            .ok_or_else(|| format!("{letter}: isn't a drive the caller mounted"))?;
        if global_drive::filesystem_name(&target).as_deref() != Some(global_drive::FILESYSTEM_NAME)
        {
            return Err(format!("{letter}: isn't a WinDroidFS drive"));
        }
        let pid = connection
            .client_pid()
            .map_err(|e| format!("couldn't identify the caller: {e}"))?;

        let mut published = lock(&self.published);
        if published.get(&letter).is_some_and(|(t, _)| *t == target) {
            return Ok(format!("{letter}: is already visible to elevated programs"));
        }
        // As SYSTEM, a drive letter lookup sees only the global namespace.
        if let Some(existing) = global_drive::target_of(letter) {
            return Err(format!("{letter}: is already in use globally ({existing})"));
        }
        global_drive::define(letter, &target)
            .map_err(|e| format!("couldn't define {letter}: globally: {e}"))?;
        published.insert(letter, (target.clone(), pid));
        drop(published);

        take_back_when_gone(self.published.clone(), letter, target, pid);
        Ok(format!("{letter}: is now visible to elevated programs"))
    }

    fn unpublish(&mut self, letter: char, connection: &Connection) -> Result<String, String> {
        sweep(&self.published);
        let pid = connection
            .client_pid()
            .map_err(|e| format!("couldn't identify the caller: {e}"))?;
        let mut published = lock(&self.published);
        match published.get(&letter) {
            None => Ok(format!("{letter}: wasn't published")),
            Some((_, owner)) if *owner != pid => {
                Err(format!("{letter}: was published for another process"))
            }
            Some((target, _)) => {
                let result = global_drive::remove(letter, target);
                published.remove(&letter);
                result
                    .map(|()| format!("{letter}: is no longer visible to elevated programs"))
                    .map_err(|e| format!("couldn't remove {letter}: globally: {e}"))
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn sweep(published: &Published) {
    lock(published).retain(|&letter, (target, _)| {
        let mounted =
            global_drive::filesystem_name(target).as_deref() == Some(global_drive::FILESYSTEM_NAME);
        if !mounted {
            let _ = global_drive::remove(letter, target);
            log(&format!("took back {letter}: (its drive is gone)"));
        }
        mounted
    });
}

fn take_back_all(published: &Published) {
    for (letter, (target, _)) in lock(published).drain() {
        let _ = global_drive::remove(letter, &target);
    }
}

fn take_back_when_gone(published: Published, letter: char, target: String, pid: u32) {
    std::thread::spawn(move || {
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if !process.is_null() {
            unsafe {
                WaitForSingleObject(process, INFINITE);
                CloseHandle(process);
            }
        }
        let mut published = lock(&published);
        if published.get(&letter) == Some(&(target.clone(), pid)) {
            let _ = global_drive::remove(letter, &target);
            published.remove(&letter);
            log(&format!("took back {letter}: (its process exited)"));
        }
    });
}

enum Identity {
    Normal,
    Accessory,
}

/// Reject any instance ID that isn't the kind of Android device this verb is allowed to touch.
fn require_identity(instance_id: &str, expected: Identity) -> Result<(), String> {
    let Some((vid, pid)) = parse_parent_instance_id(instance_id) else {
        return Err(format!("{instance_id} is not a recognizable USB device id"));
    };
    let allowed = match expected {
        Identity::Normal => is_normal_android_identity(vid, pid),
        Identity::Accessory => is_aoa_identity(vid, pid),
    };
    if allowed {
        Ok(())
    } else {
        Err(format!(
            "{vid:04X}:{pid:04X} is not an allowed target for this operation"
        ))
    }
}

/// Accept clients one at a time and answer each until `should_stop()` returns true.
fn serve(mut should_stop: impl FnMut() -> bool) {
    let mut state = ServiceState::default();
    let mut first_instance = true;

    // At startup, finish any rollback a previous run left behind.
    if let Ok(Some((id, outcome))) = driver::recover_from_journal() {
        log(&format!(
            "recovered interrupted rollback for {id}: {outcome}"
        ));
    }

    while !should_stop() {
        let mut connection: Connection = match accept_one(first_instance) {
            Ok(c) => c,
            Err(e) => {
                log(&format!("pipe accept failed: {e}"));
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        first_instance = false;

        // A stop request wakes us by connecting and hanging up straight away, so check the flag
        // before treating a dead connection as a malformed request.
        if should_stop() {
            break;
        }

        match connection.read_request() {
            Ok(request) => {
                let response = state.handle(request, &connection);
                if let Response::Err { message } = &response {
                    log(&format!("request failed: {message}"));
                }
                let _ = connection.respond(&response);
            }
            Err(e) => {
                let _ = connection.respond(&Response::Err {
                    message: format!("bad request: {e}"),
                });
            }
        }
    }

    take_back_all(&state.published);
}

fn run_foreground() {
    // Ctrl+C (or closing the window) mid-swap must restore the driver before the process dies,
    // otherwise the phone is left on WinUSB until the next start recovers from the journal.
    // The Windows-service path gets this from its SCM stop handler instead.
    driver::install_ctrl_guard();

    // Serve either way. Without elevation the pipe, the protocol and the request policy all
    // work and can be exercised; only the driver calls themselves fail, with a clear error.
    // Refusing to start would make the whole thing untestable without an admin terminal.
    if driver::is_elevated() {
        println!("WinDroidFS driver service listening on the pipe. Ctrl+C to stop.");
    } else {
        println!("WinDroidFS driver service listening on the pipe (NOT elevated).");
        println!("   -> Driver changes will be refused; everything else works.");
        println!("   -> Re-run from an Administrator terminal to switch drivers.");
    }
    serve(|| false);
}

fn log(message: &str) {
    // Foreground: stderr. As a Windows service there's no console; this is where a future
    // version would write to the Windows event log.
    eprintln!("[windroidfs-service] {message}");
}

// ===========================================================================
// Windows service plumbing
// ===========================================================================

mod service_entry {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{
        self, ServiceControlHandlerResult, ServiceStatusHandle,
    };

    use super::SERVICE_NAME;

    /// Set once `register` returns. The stop handler needs it to report StopPending, but the
    /// handler has to be built *before* registering, hence the cell.
    static STATUS_HANDLE: OnceLock<ServiceStatusHandle> = OnceLock::new();

    fn set_state(
        handle: &ServiceStatusHandle,
        state: ServiceState,
        accept: ServiceControlAccept,
        wait_hint: Duration,
    ) -> windows_service::Result<()> {
        handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint,
            process_id: None,
        })
    }

    windows_service::define_windows_service!(ffi_service_main, service_main);

    pub fn run() -> Result<(), windows_service::Error> {
        // Hands control to `service_main` on a dedicated thread.
        windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main)
    }

    fn service_main(_args: Vec<std::ffi::OsString>) {
        if let Err(e) = run_service() {
            super::log(&format!("service stopped with error: {e}"));
        }
    }

    fn run_service() -> Result<(), windows_service::Error> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_handler = stop.clone();

        let handler = move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                stop_for_handler.store(true, Ordering::SeqCst);

                // Tell the SCM we're on our way out. A rollback may still have to run, so ask
                // for a little patience rather than letting it assume we've hung.
                if let Some(handle) = STATUS_HANDLE.get() {
                    let _ = set_state(
                        handle,
                        ServiceState::StopPending,
                        ServiceControlAccept::empty(),
                        Duration::from_secs(15),
                    );
                }

                // The serve loop is blocked waiting for a client, so poke it: it wakes, sees the
                // flag and returns. Without this, stopping would wait for the next real request.
                windroidfs::ipc::transport::wake_server();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };

        let status_handle = service_control_handler::register(SERVICE_NAME, handler)?;
        let _ = STATUS_HANDLE.set(status_handle);

        set_state(
            &status_handle,
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            Duration::default(),
        )?;

        super::serve(|| stop.load(Ordering::SeqCst));

        set_state(
            &status_handle,
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            Duration::default(),
        )?;
        Ok(())
    }
}

// ===========================================================================
// Install / uninstall / start / stop
// ===========================================================================

mod manage {
    use std::ffi::OsString;

    use windows_service::service::{
        ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState,
        ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_sys::Win32::Foundation::ERROR_SERVICE_EXISTS;

    use super::{SERVICE_DISPLAY, SERVICE_NAME, report_restore};

    fn manager(access: ServiceManagerAccess) -> Result<ServiceManager, String> {
        ServiceManager::local_computer(None::<&str>, access).map_err(|e| e.to_string())
    }

    pub fn install() -> Result<String, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let manager =
            manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;

        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(SERVICE_DISPLAY),
            service_type: ServiceType::OWN_PROCESS,
            // Starts at boot. Change to OnDemand if you'd rather the daemon start it.
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe,
            launch_arguments: vec![], // no args => the SCM entry point
            dependencies: vec![],
            account_name: None, // None = LocalSystem
            account_password: None,
        };

        let access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START;
        let (service, verb) = match manager.create_service(&info, access) {
            Ok(service) => (service, "Installed"),
            Err(windows_service::Error::Winapi(e))
                if e.raw_os_error() == Some(ERROR_SERVICE_EXISTS as i32) =>
            {
                let service = manager
                    .open_service(SERVICE_NAME, access)
                    .map_err(|e| e.to_string())?;
                service.change_config(&info).map_err(|e| e.to_string())?;
                (service, "Updated")
            }
            Err(e) => return Err(e.to_string()),
        };
        let _ = service.set_description(
            "Switches Android USB drivers to WinUSB for WinDroidFS and restores them afterwards.",
        );
        service.start::<&str>(&[]).map_err(|e| e.to_string())?;
        Ok(format!("{verb} and started {SERVICE_DISPLAY}."))
    }

    pub fn uninstall() -> Result<String, String> {
        // Put drivers back before the service goes away, otherwise a swapped device could be
        // left on WinUSB with nothing left to restore it.
        let restored = report_restore();

        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let service = manager
            .open_service(
                SERVICE_NAME,
                ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
            )
            .map_err(|e| e.to_string())?;

        if let Ok(status) = service.query_status()
            && status.current_state != ServiceState::Stopped
        {
            let _ = service.stop();
        }
        service.delete().map_err(|e| e.to_string())?;
        Ok(format!("{restored}\nRemoved {SERVICE_DISPLAY}."))
    }

    pub fn start() -> Result<String, String> {
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let service = manager
            .open_service(SERVICE_NAME, ServiceAccess::START)
            .map_err(|e| e.to_string())?;
        service.start::<&str>(&[]).map_err(|e| e.to_string())?;
        Ok("Service started.".into())
    }

    pub fn stop() -> Result<String, String> {
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let service = manager
            .open_service(SERVICE_NAME, ServiceAccess::STOP)
            .map_err(|e| e.to_string())?;
        service.stop().map_err(|e| e.to_string())?;
        Ok("Service stopped.".into())
    }
}

/// Run the "restore every swapped driver" pass directly (used during uninstall), and summarise it.
fn report_restore() -> String {
    if !driver::is_elevated() {
        return "Skipped driver restore (not elevated).".into();
    }
    let _ = driver::recover_from_journal();
    match driver::restore_android_drivers(ANDROID_VIDS, is_aoa_identity) {
        Ok(list) if list.is_empty() => "No drivers needed restoring.".into(),
        Ok(list) => format!("Restored {} device(s).", list.len()),
        Err(e) => format!("Driver restore failed: {e}"),
    }
}
