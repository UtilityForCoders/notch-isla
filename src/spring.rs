//! Resorte amortiguado: da a la isla ese movimiento con un pequeño rebote.

pub struct Spring {
    pub value: f32,
    velocity: f32,
    target: f32,
    stiffness: f32,
    damping: f32,
}

impl Spring {
    /// `stiffness` controla la rapidez; `damping` cuánto rebota (menos = más rebote).
    pub fn new(value: f32, stiffness: f32, damping: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            stiffness,
            damping,
        }
    }

    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    /// Cambia rapidez y rebote sin cortar el movimiento (sigue desde donde va).
    pub fn retune(&mut self, stiffness: f32, damping: f32) {
        self.stiffness = stiffness;
        self.damping = damping;
    }

    /// Salta a `value` sin animación y queda quieto ahí.
    pub fn reset(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    /// Avanza la simulación `dt` segundos. Devuelve `true` mientras siga en movimiento.
    pub fn step(&mut self, dt: f32) -> bool {
        // Limita saltos grandes (p. ej. si la PC se trabó) y subdivide para estabilidad.
        let mut remaining = dt.min(0.1);
        while remaining > 0.0 {
            let h = remaining.min(1.0 / 240.0);
            let force = self.stiffness * (self.target - self.value) - self.damping * self.velocity;
            self.velocity += force * h;
            self.value += self.velocity * h;
            remaining -= h;
        }

        let settled = (self.target - self.value).abs() < 0.0005 && self.velocity.abs() < 0.005;
        if settled {
            self.value = self.target;
            self.velocity = 0.0;
        }
        !settled
    }
}

#[cfg(test)]
mod tests {
    use super::Spring;

    /// Con el resorte de encoger (el de `main.rs`), la isla baja de 1 a 0 sin pasarse de largo
    /// y en menos de un segundo.
    #[test]
    fn encoge_sin_rebote() {
        let mut s = Spring::new(1.0, 110.0, 21.0);
        s.set_target(0.0);
        let (mut t, mut lowest) = (0.0, 1.0f32);
        while s.step(1.0 / 60.0) {
            lowest = lowest.min(s.value);
            t += 1.0 / 60.0;
            assert!(t < 1.5, "no se asienta");
        }
        assert!(lowest > -0.01, "rebota: llega a {lowest}");
        println!("se asienta en {t:.2} s");
    }
}
