//! Meta diaria de agua: cuánto se lleva tomado hoy. El conteo se guarda en el registro junto
//! a los ajustes y vuelve a cero al cambiar el día. Los recordatorios los pone la ventana.

use windows::core::w;
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::settings;

/// Lo que suma cada vaso anotado, en mililitros.
pub const GLASS_ML: u32 = 250;
/// Meta diaria: límites y paso del deslizador, en mililitros.
pub const GOAL_STEP_ML: u32 = 250;
pub const GOAL_MIN_ML: u32 = 1000;
pub const GOAL_MAX_ML: u32 = 4000;
/// Recordatorios: límites y paso del deslizador, en minutos.
pub const INTERVAL_STEP_MIN: u32 = 15;
pub const INTERVAL_MIN_MIN: u32 = 15;
pub const INTERVAL_MAX_MIN: u32 = 120;

/// Día local como número (20261116), para saber cuándo empezar de cero.
pub fn today() -> u32 {
    let now = unsafe { GetLocalTime() };
    u32::from(now.wYear) * 10_000 + u32::from(now.wMonth) * 100 + u32::from(now.wDay)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Water {
    day: u32,
    pub ml: u32,
}

impl Water {
    /// El conteo guardado, o cero si es de otro día.
    pub fn load() -> Self {
        let mut water = Self {
            day: settings::read_dword(w!("WaterDay")).unwrap_or(0),
            ml: settings::read_dword(w!("WaterMl")).unwrap_or(0),
        };
        water.roll(today());
        water
    }

    fn save(&self) {
        settings::write_dword(w!("WaterDay"), self.day);
        settings::write_dword(w!("WaterMl"), self.ml);
    }

    /// Empieza de cero si `today` ya no es el día del conteo. Devuelve si cambió.
    fn roll(&mut self, today: u32) -> bool {
        if self.day == today {
            return false;
        }
        *self = Self { day: today, ml: 0 };
        true
    }

    /// Pone el conteo al día (por si pasó la medianoche con la app abierta).
    pub fn refresh(&mut self) {
        if self.roll(today()) {
            self.save();
        }
    }

    /// Anota un vaso. Devuelve si con este vaso se alcanzó la meta `goal_ml`.
    pub fn add_glass(&mut self, goal_ml: u32) -> bool {
        self.roll(today());
        let reached = self.add(goal_ml);
        self.save();
        reached
    }

    fn add(&mut self, goal_ml: u32) -> bool {
        let before = self.ml;
        // Tope generoso: evita que un conteo absurdo desborde.
        self.ml = (self.ml + GLASS_ML).min(20_000);
        before < goal_ml && self.ml >= goal_ml
    }

    pub fn reset(&mut self) {
        *self = Self {
            day: today(),
            ml: 0,
        };
        self.save();
    }

    /// "750 de 2000 ml".
    pub fn progress(&self, goal_ml: u32) -> String {
        format!("{} de {goal_ml} ml", self.ml)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_conteo_vuelve_a_cero_al_cambiar_el_dia() {
        let mut water = Water {
            day: 20261115,
            ml: 1500,
        };
        assert!(!water.roll(20261115));
        assert_eq!(water.ml, 1500);
        assert!(water.roll(20261116));
        assert_eq!(
            water,
            Water {
                day: 20261116,
                ml: 0
            }
        );
    }

    #[test]
    fn la_meta_se_avisa_una_sola_vez() {
        let mut water = Water {
            day: 20261116,
            ml: 0,
        };
        let reached: Vec<bool> = (0..5).map(|_| water.add(1000)).collect();
        assert_eq!(reached, [false, false, false, true, false]);
        assert_eq!(water.progress(1000), "1250 de 1000 ml");
    }

    #[test]
    fn el_dia_local_tiene_forma_de_fecha() {
        let day = today();
        assert!((20240101..21000101).contains(&day));
        assert!((1..=12).contains(&(day / 100 % 100)));
        assert!((1..=31).contains(&(day % 100)));
    }
}
