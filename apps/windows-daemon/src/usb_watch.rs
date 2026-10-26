use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_NOTIFY_ACTION, CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER,
    CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CM_Register_Notification, CM_Unregister_Notification,
    CR_SUCCESS, HCMNOTIFICATION,
};
use windows_sys::core::GUID;

const GUID_DEVINTERFACE_USB_DEVICE: GUID = GUID::from_u128(0xA5DCBF10_6530_11D2_901F_00C04FB951ED);

/// A flag the callback raises and [`UsbWatch::wait`] lowers.
#[derive(Default)]
struct Signal {
    raised: Mutex<bool>,
    changed: Condvar,
}

impl Signal {
    fn raise(&self) {
        *self.raised.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.changed.notify_all();
    }
}

pub struct UsbWatch {
    signal: Arc<Signal>,
    /// Null if registering failed; [`UsbWatch::wait`] then only ever times out.
    registration: HCMNOTIFICATION,
    /// The callback's own reference to `signal` (see [`UsbWatch::new`]), given back on drop.
    context: *const Signal,
}

// The raw pointers are only used to unregister and release in `drop`. Everything shared between threads goes through `Signal`, which is Sync.
unsafe impl Send for UsbWatch {}
unsafe impl Sync for UsbWatch {}

impl UsbWatch {
    /// Start listening. If Windows refuses, this still works as a timer.
    pub fn new() -> UsbWatch {
        let signal = Arc::new(Signal::default());
        // The callback runs on a Windows thread pool thread at any time until unregistered. It gets a reference of its own, released in `drop` after unregistering.
        let context = Arc::into_raw(signal.clone());

        let mut filter: CM_NOTIFY_FILTER = unsafe { std::mem::zeroed() };
        filter.cbSize = size_of::<CM_NOTIFY_FILTER>() as u32;
        filter.FilterType = CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE;
        filter.u.DeviceInterface.ClassGuid = GUID_DEVINTERFACE_USB_DEVICE;

        let mut registration: HCMNOTIFICATION = std::ptr::null_mut();
        let result = unsafe {
            CM_Register_Notification(&filter, context.cast(), Some(on_change), &mut registration)
        };
        if result != CR_SUCCESS {
            log::warn!(
                "⚠️  Can't watch for USB devices (error {result}); checking periodically instead."
            );
            registration = std::ptr::null_mut();
        }

        UsbWatch {
            signal,
            registration,
            context,
        }
    }

    /// Wait until a USB device arrives or leaves, [`UsbWatch::wake`] is called, or `timeout` passes. True unless it timed out.
    pub fn wait(&self, timeout: Duration) -> bool {
        let raised = self.signal.raised.lock().unwrap_or_else(|e| e.into_inner());
        let (mut raised, _) = self
            .signal
            .changed
            .wait_timeout_while(raised, timeout, |raised| !*raised)
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *raised)
    }

    /// End a [`UsbWatch::wait`] early, as if a device had changed. For "look again now".
    pub fn wake(&self) {
        self.signal.raise();
    }
}

impl Default for UsbWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for UsbWatch {
    fn drop(&mut self) {
        if !self.registration.is_null() {
            // Once this returns, the callback won't run again.
            unsafe { CM_Unregister_Notification(self.registration) };
        }
        unsafe { drop(Arc::from_raw(self.context)) };
    }
}

unsafe extern "system" fn on_change(
    _registration: HCMNOTIFICATION,
    context: *const c_void,
    _action: CM_NOTIFY_ACTION,
    _event: *const CM_NOTIFY_EVENT_DATA,
    _event_size: u32,
) -> u32 {
    let signal = unsafe { &*context.cast::<Signal>() };
    signal.raise();
    0 // ERROR_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn waits_until_woken_or_timed_out() {
        let watch = UsbWatch::new();
        let started = Instant::now();
        let woke = watch.wait(Duration::from_millis(100));
        assert!(woke || started.elapsed() >= Duration::from_millis(100));

        // A wake before the wait still counts, and is used up by it.
        watch.wake();
        assert!(watch.wait(Duration::from_secs(5)));

        // A wake from another thread ends a wait in progress.
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                watch.wake();
            });
            let started = Instant::now();
            assert!(watch.wait(Duration::from_secs(5)));
            assert!(started.elapsed() < Duration::from_secs(5));
        });
    }
}
