//! Temporizador, cronómetro y Pomodoro: solo las cuentas. La ventana decide cuándo despertar
//! (`COUNTDOWN_TIMER`) y cómo avisar; aquí no hay nada de Windows, así se puede probar.

use std::time::{Duration, Instant};

/// Duración libre más larga que se puede elegir, en minutos.
pub const MAX_MINUTES: u32 = 180;

/// Pomodoro clásico: 25 minutos de foco y 5 de descanso; cada 4 focos, un descanso largo.
const FOCUS_MINUTES: u32 = 25;
const BREAK_MINUTES: u32 = 5;
const LONG_BREAK_MINUTES: u32 = 15;
const ROUNDS: u32 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Focus,
    Break,
    LongBreak,
}

impl Phase {
    fn minutes(self) -> u32 {
        match self {
            Phase::Focus => FOCUS_MINUTES,
            Phase::Break => BREAK_MINUTES,
            Phase::LongBreak => LONG_BREAK_MINUTES,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Countdown,
    /// Cuenta hacia arriba hasta que se detiene.
    Stopwatch,
    /// `round`: número de foco dentro del ciclo (1 a 4).
    Pomodoro {
        phase: Phase,
        round: u32,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct Timer {
    pub kind: Kind,
    started: Instant,
    /// Minutos de la cuenta regresiva; 0 en el cronómetro.
    minutes: u32,
}

/// Lo que hay que avisar cuando una cuenta llega a cero, y la cuenta que sigue (Pomodoro).
pub struct Finished {
    pub title: &'static str,
    pub subtitle: String,
    pub next: Option<Timer>,
}

impl Timer {
    pub fn countdown(minutes: u32, now: Instant) -> Self {
        Self {
            kind: Kind::Countdown,
            started: now,
            minutes: minutes.clamp(1, MAX_MINUTES),
        }
    }

    pub fn stopwatch(now: Instant) -> Self {
        Self {
            kind: Kind::Stopwatch,
            started: now,
            minutes: 0,
        }
    }

    pub fn pomodoro(now: Instant) -> Self {
        Self::phase(Phase::Focus, 1, now)
    }

    fn phase(phase: Phase, round: u32, now: Instant) -> Self {
        Self {
            kind: Kind::Pomodoro { phase, round },
            started: now,
            minutes: phase.minutes(),
        }
    }

    pub fn is_stopwatch(&self) -> bool {
        self.kind == Kind::Stopwatch
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
    }

    /// Lo que falta para terminar; `None` en el cronómetro, que no termina solo.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        (!self.is_stopwatch()).then(|| {
            Duration::from_secs(u64::from(self.minutes) * 60).saturating_sub(self.elapsed(now))
        })
    }

    fn name(&self) -> &'static str {
        match self.kind {
            Kind::Countdown => "Temporizador",
            Kind::Stopwatch => "Cronómetro",
            Kind::Pomodoro { phase, .. } => match phase {
                Phase::Focus => "Foco",
                Phase::Break => "Descanso",
                Phase::LongBreak => "Descanso largo",
            },
        }
    }

    /// Texto de la píldora bajo la línea de reposo. Va en minutos para no redibujar cada
    /// segundo: cambia una vez por minuto (ver `next_change`).
    pub fn pill(&self, now: Instant) -> String {
        let minutes = match self.remaining(now) {
            Some(left) => left.as_secs().div_ceil(60),
            None => self.elapsed(now).as_secs() / 60,
        };
        match (self.is_stopwatch(), minutes) {
            (false, 0 | 1) => format!("{} · menos de 1 min", self.name()),
            (true, 0) => format!("{} · en marcha", self.name()),
            _ => format!("{} · {minutes} min", self.name()),
        }
    }

    /// Cuánto falta para que cambie el texto de `pill`.
    pub fn next_change(&self, now: Instant) -> Duration {
        let into_minute = match self.remaining(now) {
            // Cambia cuando lo que falta baja de un minuto entero.
            Some(left) => left.as_millis() % 60_000,
            None => 60_000 - self.elapsed(now).as_millis() % 60_000,
        };
        Duration::from_millis(if into_minute == 0 {
            60_000
        } else {
            into_minute as u64
        })
    }

    /// Estado con el tiempo exacto, para el menú: "Foco 2 de 4 · quedan 24:13".
    pub fn status(&self, now: Instant) -> String {
        let name = match self.kind {
            Kind::Pomodoro {
                phase: Phase::Focus,
                round,
            } => format!("Foco {round} de {ROUNDS}"),
            _ => self.name().to_string(),
        };
        match self.remaining(now) {
            Some(left) => format!("{name} · quedan {}", clock(left)),
            None => format!("{name} · {}", clock(self.elapsed(now))),
        }
    }

    /// La cuenta llegó a cero: qué avisar y, en Pomodoro, la fase que empieza.
    pub fn finished(&self, now: Instant) -> Finished {
        match self.kind {
            Kind::Countdown | Kind::Stopwatch => Finished {
                title: "Se acabó el tiempo",
                subtitle: format!("Temporizador de {}", minutes_text(self.minutes)),
                next: None,
            },
            Kind::Pomodoro {
                phase: Phase::Focus,
                round,
            } => {
                let rest = if round >= ROUNDS {
                    Phase::LongBreak
                } else {
                    Phase::Break
                };
                Finished {
                    title: "Terminó el foco",
                    subtitle: format!("Descansa {}", minutes_text(rest.minutes())),
                    next: Some(Self::phase(rest, round, now)),
                }
            }
            Kind::Pomodoro { round, .. } => {
                let round = if round >= ROUNDS { 1 } else { round + 1 };
                Finished {
                    title: "Terminó el descanso",
                    subtitle: format!("Foco {round} de {ROUNDS}: {}", minutes_text(FOCUS_MINUTES)),
                    next: Some(Self::phase(Phase::Focus, round, now)),
                }
            }
        }
    }
}

/// Resumen del Pomodoro para el menú.
pub fn pomodoro_summary() -> String {
    format!("{FOCUS_MINUTES} min de foco y {BREAK_MINUTES} de descanso")
}

pub fn minutes_text(minutes: u32) -> String {
    if minutes == 1 {
        "1 minuto".to_string()
    } else {
        format!("{minutes} minutos")
    }
}

/// "m:ss", o "h:mm:ss" desde una hora.
pub fn clock(time: Duration) -> String {
    let secs = time.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    #[test]
    fn cuenta_regresiva_informa_lo_que_falta() {
        let start = Instant::now();
        let timer = Timer::countdown(10, start);
        assert_eq!(timer.remaining(start), Some(Duration::from_secs(600)));
        assert_eq!(timer.pill(start), "Temporizador · 10 min");
        assert_eq!(timer.status(after(start, 61)), "Temporizador · quedan 8:59");
        // 9:30 restantes se muestran como 10 min y cambian en 30 s.
        assert_eq!(timer.pill(after(start, 30)), "Temporizador · 10 min");
        assert_eq!(timer.next_change(after(start, 30)), Duration::from_secs(30));
        assert_eq!(
            timer.pill(after(start, 570)),
            "Temporizador · menos de 1 min"
        );
        assert_eq!(timer.remaining(after(start, 900)), Some(Duration::ZERO));
        assert!(timer.finished(start).next.is_none());
    }

    #[test]
    fn duracion_libre_se_limita() {
        let start = Instant::now();
        let secs = |minutes| Timer::countdown(minutes, start).remaining(start).unwrap();
        assert_eq!(secs(0), Duration::from_secs(60));
        assert_eq!(secs(999), Duration::from_secs(u64::from(MAX_MINUTES) * 60));
    }

    #[test]
    fn cronometro_cuenta_hacia_arriba_y_no_termina() {
        let start = Instant::now();
        let timer = Timer::stopwatch(start);
        assert_eq!(timer.remaining(after(start, 5000)), None);
        assert_eq!(timer.pill(after(start, 20)), "Cronómetro · en marcha");
        assert_eq!(timer.pill(after(start, 125)), "Cronómetro · 2 min");
        assert_eq!(timer.status(after(start, 3725)), "Cronómetro · 1:02:05");
        assert_eq!(
            timer.next_change(after(start, 125)),
            Duration::from_secs(55)
        );
    }

    #[test]
    fn pomodoro_alterna_foco_y_descanso_con_uno_largo_cada_cuatro() {
        let start = Instant::now();
        let mut timer = Timer::pomodoro(start);
        let mut phases = Vec::new();
        for _ in 0..9 {
            phases.push(timer.kind);
            timer = timer.finished(start).next.expect("el Pomodoro sigue solo");
        }
        let focus = |round| Kind::Pomodoro {
            phase: Phase::Focus,
            round,
        };
        let rest = |phase, round| Kind::Pomodoro { phase, round };
        assert_eq!(
            phases,
            [
                focus(1),
                rest(Phase::Break, 1),
                focus(2),
                rest(Phase::Break, 2),
                focus(3),
                rest(Phase::Break, 3),
                focus(4),
                rest(Phase::LongBreak, 4),
                focus(1),
            ]
        );
        assert_eq!(
            Timer::pomodoro(start).status(start),
            "Foco 1 de 4 · quedan 25:00"
        );
    }
}
