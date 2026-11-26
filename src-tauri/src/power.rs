/// An idle-display-sleep assertion owned by this process, released on exit by macOS.
#[derive(Default)]
pub struct KeepAwake {
    assertion: Option<u32>,
}

impl KeepAwake {
    pub fn enabled(&self) -> bool {
        self.assertion.is_some()
    }

    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), String> {
        if enabled == self.enabled() {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        {
            if enabled {
                self.assertion = Some(native::create()?);
            } else if let Some(id) = self.assertion {
                native::release(id)?;
                self.assertion = None;
            }
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        Err("keep_awake_unsupported".to_owned())
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        let _ = self.set_enabled(false);
    }
}

#[cfg(target_os = "macos")]
mod native {
    use std::ffi::{c_char, c_void};

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            value: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFRelease(value: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: *const c_void,
            level: u32,
            name: *const c_void,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    pub fn create() -> Result<u32, String> {
        // SAFETY: NUL-terminated strings, UTF-8 encoding, and a writable assertion ID.
        unsafe {
            let kind = CFStringCreateWithCString(
                std::ptr::null(),
                c"PreventUserIdleDisplaySleep".as_ptr(),
                0x08000100,
            );
            let reason = CFStringCreateWithCString(
                std::ptr::null(),
                c"Trove: keep the screen awake".as_ptr(),
                0x08000100,
            );
            let mut id = 0;
            let result = if kind.is_null() || reason.is_null() {
                -1
            } else {
                IOPMAssertionCreateWithName(kind, 255, reason, &mut id)
            };
            if !kind.is_null() {
                CFRelease(kind);
            }
            if !reason.is_null() {
                CFRelease(reason);
            }
            if result == 0 {
                Ok(id)
            } else {
                Err("keep_awake_unavailable".to_owned())
            }
        }
    }

    pub fn release(id: u32) -> Result<(), String> {
        // SAFETY: the ID came from a successful assertion creation and is still owned here.
        if unsafe { IOPMAssertionRelease(id) } == 0 {
            Ok(())
        } else {
            Err("keep_awake_unavailable".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default_and_disabling_is_idempotent() {
        let mut power = KeepAwake::default();
        assert!(!power.enabled());
        assert!(power.set_enabled(false).is_ok());
        assert!(!power.enabled());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn enabling_is_idempotent_and_disabling_releases_assertion() {
        let mut power = KeepAwake::default();
        power.set_enabled(true).unwrap();
        let id = power.assertion;
        assert!(power.enabled());
        power.set_enabled(true).unwrap();
        assert_eq!(power.assertion, id);
        power.set_enabled(false).unwrap();
        assert!(!power.enabled());
    }
}
