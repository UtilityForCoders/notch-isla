//! Espera corta de alta resolución sin cambiar la resolución global de los temporizadores.
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    CreateWaitableTimerExW, SetWaitableTimerEx, WaitForSingleObject,
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE, TIMER_ALL_ACCESS,
};

pub struct FrameWait(Option<HANDLE>);

impl FrameWait {
    pub fn new() -> Self {
        Self(unsafe {
            CreateWaitableTimerExW(
                None,
                None,
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
            .ok()
        })
    }

    pub fn wait(&self, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        if let Some(timer) = self.0 {
            let due = -(duration.as_nanos().div_ceil(100).min(i64::MAX as u128) as i64);
            unsafe {
                if SetWaitableTimerEx(timer, &due, 0, None, None, None, 0).is_ok() {
                    let _ = WaitForSingleObject(timer, INFINITE);
                    return;
                }
            }
        }
        std::thread::sleep(duration);
    }
}

impl Drop for FrameWait {
    fn drop(&mut self) {
        if let Some(timer) = self.0 {
            unsafe {
                let _ = CloseHandle(timer);
            }
        }
    }
}
