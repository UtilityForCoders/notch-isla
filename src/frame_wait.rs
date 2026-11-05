//! Espera corta de alta resolución sin cambiar la resolución global de los temporizadores.
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    CreateWaitableTimerExW, SetWaitableTimerEx, WaitForSingleObject,
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE, TIMER_ALL_ACCESS,
};

pub struct FrameWait(Option<HANDLE>);

/// Conserva los plazos entre cuadros aunque el temporizador despierte tarde.
/// Una espera corta de DWM puede ser un VSync válido: importa el tiempo desde
/// el cuadro anterior, no cuánto duró una sola llamada a DwmFlush.
#[derive(Default)]
pub struct FrameSchedule {
    interval: Duration,
    deadline: Option<Instant>,
    last_release: Option<Instant>,
}

impl FrameSchedule {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn delay(&mut self, now: Instant, interval: Duration, synced: bool) -> Duration {
        let interval = interval.max(Duration::from_nanos(1));
        if self.interval != interval {
            self.reset();
            self.interval = interval;
        }
        if synced
            && self
                .last_release
                .is_none_or(|last| now.saturating_duration_since(last) >= interval / 2)
        {
            self.last_release = Some(now);
            self.deadline = Some(now + interval);
            return Duration::ZERO;
        }
        let deadline = self.deadline.unwrap_or(now + interval);
        let release = now.max(deadline);
        self.last_release = Some(release);
        self.deadline = Some(next_deadline(deadline, release, interval));
        deadline.saturating_duration_since(now)
    }
}

/// Salta plazos vencidos sin acumular retraso ni producir una ráfaga de cuadros.
pub fn next_deadline(deadline: Instant, now: Instant, interval: Duration) -> Instant {
    let interval = interval.max(Duration::from_nanos(1));
    let steps = now.saturating_duration_since(deadline).as_nanos() / interval.as_nanos() + 1;
    deadline + Duration::from_nanos((interval.as_nanos() * steps).min(u64::MAX as u128) as u64)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_vsync_corto_no_agrega_otro_retraso() {
        let start = Instant::now();
        let interval = Duration::from_nanos(1_000_000_000 / 144);
        let mut schedule = FrameSchedule::default();
        assert_eq!(schedule.delay(start, interval, true), Duration::ZERO);
        // El hilo puede volver a DwmFlush justo antes del siguiente refresco.
        assert_eq!(
            schedule.delay(start + interval, interval, true),
            Duration::ZERO
        );
        assert_eq!(
            schedule.delay(start + interval * 2, interval, true),
            Duration::ZERO
        );
    }

    #[test]
    fn la_espera_de_respaldo_no_acumula_la_tardanza() {
        let start = Instant::now();
        let interval = Duration::from_nanos(1_000_000_000 / 240);
        let late = Duration::from_micros(300);
        let mut schedule = FrameSchedule::default();
        assert_eq!(schedule.delay(start, interval, false), interval);
        for i in 1..200 {
            assert_eq!(
                schedule.delay(start + interval * i + late, interval, false),
                interval - late
            );
        }
    }

    #[test]
    fn retornos_inmediatos_no_generan_cuadros_sin_limite() {
        let start = Instant::now();
        let interval = Duration::from_millis(4);
        let mut schedule = FrameSchedule::default();
        assert_eq!(schedule.delay(start, interval, true), Duration::ZERO);
        assert_eq!(schedule.delay(start, interval, true), interval);
        assert_eq!(schedule.delay(start + interval, interval, true), interval);
        // Un bloqueo largo descarta plazos perdidos y no intenta recuperarlos en ráfaga.
        let resumed = start + Duration::from_secs(2);
        assert_eq!(schedule.delay(resumed, interval, false), Duration::ZERO);
        assert_eq!(schedule.delay(resumed, interval, false), interval);
    }

    #[test]
    fn cambiar_hz_o_reanudar_descarta_el_plazo_anterior() {
        let start = Instant::now();
        let mut schedule = FrameSchedule::default();
        schedule.delay(start, Duration::from_millis(7), false);
        assert_eq!(
            schedule.delay(start, Duration::from_millis(4), false),
            Duration::from_millis(4)
        );
        schedule.reset();
        assert_eq!(
            schedule.delay(start, Duration::from_millis(4), true),
            Duration::ZERO
        );
    }
}
