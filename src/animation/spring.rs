use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct SpringParams {
    pub damping: f64,
    pub mass: f64,
    pub stiffness: f64,
    pub epsilon: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub from: f64,
    pub to: f64,
    pub initial_velocity: f64,
    pub params: SpringParams,
}

impl SpringParams {
    pub fn new(damping_ratio: f64, stiffness: f64, epsilon: f64) -> Self {
        let damping_ratio = damping_ratio.max(0.);
        let stiffness = stiffness.max(0.);
        let epsilon = epsilon.max(0.);

        let mass = 1.;
        let critical_damping = 2. * (mass * stiffness).sqrt();
        let damping = damping_ratio * critical_damping;

        Self {
            damping,
            mass,
            stiffness,
            epsilon,
        }
    }
}

impl Spring {
    pub fn value_at(&self, t: Duration) -> f64 {
        self.oscillate(t.as_secs_f64())
    }

    // Based on libadwaita (LGPL-2.1-or-later):
    // https://gitlab.gnome.org/GNOME/libadwaita/-/blob/1.4.4/src/adw-spring-animation.c,
    // which itself is based on (MIT):
    // https://github.com/robb/RBBAnimation/blob/master/RBBAnimation/RBBSpringAnimation.m
    /// Computes and returns the duration until the spring is at rest.
    pub fn duration(&self) -> Duration {
        const DELTA: f64 = 0.001;

        let beta = self.params.damping / (2. * self.params.mass);

        if beta.abs() <= f64::EPSILON || beta < 0. {
            return Duration::MAX;
        }

        if (self.to - self.from).abs() <= f64::EPSILON {
            return Duration::ZERO;
        }

        let omega0 = (self.params.stiffness / self.params.mass).sqrt();

        // As first ansatz for the overdamped solution,
        // and general estimation for the oscillating ones
        // we take the value of the envelope when it's < epsilon.
        let mut x0 = -self.params.epsilon.ln() / beta;

        // f64::EPSILON is too small for this specific comparison, so we use
        // f32::EPSILON even though it's doubles.
        if (beta - omega0).abs() <= f64::from(f32::EPSILON) || beta < omega0 {
            return Duration::from_secs_f64(x0);
        }

        // Since the overdamped solution decays way slower than the envelope
        // we need to use the value of the oscillation itself.
        // Newton's root finding method is a good candidate in this particular case:
        // https://en.wikipedia.org/wiki/Newton%27s_method
        let mut y0 = self.oscillate(x0);
        let m = (self.oscillate(x0 + DELTA) - y0) / DELTA;

        let mut x1 = (self.to - y0 + m * x0) / m;
        let mut y1 = self.oscillate(x1);

        let mut i = 0;
        while (self.to - y1).abs() > self.params.epsilon {
            if i > 1000 {
                return Duration::ZERO;
            }

            x0 = x1;
            y0 = y1;

            let m = (self.oscillate(x0 + DELTA) - y0) / DELTA;

            x1 = (self.to - y0 + m * x0) / m;
            y1 = self.oscillate(x1);

            // Overdamped springs have some numerical stability issues...
            if !y1.is_finite() {
                return Duration::from_secs_f64(x0);
            }

            i += 1;
        }

        Duration::from_secs_f64(x1)
    }

    /// Computes and returns the duration until the spring reaches its target position.
    pub fn clamped_duration(&self) -> Option<Duration> {
        let beta = self.params.damping / (2. * self.params.mass);

        if beta.abs() <= f64::EPSILON || beta < 0. {
            return Some(Duration::MAX);
        }

        if (self.to - self.from).abs() <= f64::EPSILON {
            return Some(Duration::ZERO);
        }

        // The first frame is not that important and we avoid finding the trivial 0 for in-place
        // animations.
        let mut i = 1u16;
        let mut y = self.oscillate(f64::from(i) / 1000.);

        while (self.to - self.from > f64::EPSILON && self.to - y > self.params.epsilon)
            || (self.from - self.to > f64::EPSILON && y - self.to > self.params.epsilon)
        {
            if i > 3000 {
                return None;
            }

            i += 1;
            y = self.oscillate(f64::from(i) / 1000.);
        }

        Some(Duration::from_millis(u64::from(i)))
    }

    /// Returns the spring position at a given time in seconds.
    fn oscillate(&self, t: f64) -> f64 {
        let b = self.params.damping;
        let m = self.params.mass;
        let k = self.params.stiffness;
        let v0 = self.initial_velocity;

        let beta = b / (2. * m);
        let omega0 = (k / m).sqrt();

        let x0 = self.from - self.to;

        let envelope = (-beta * t).exp();

        // Solutions of the form C1*e^(lambda1*x) + C2*e^(lambda2*x)
        // for the differential equation m*ẍ+b*ẋ+kx = 0

        // f64::EPSILON is too small for this specific comparison, so we use
        // f32::EPSILON even though it's doubles.
        if (beta - omega0).abs() <= f64::from(f32::EPSILON) {
            // Critically damped.
            self.to + envelope * (x0 + (beta * x0 + v0) * t)
        } else if beta < omega0 {
            // Underdamped.
            let omega1 = ((omega0 * omega0) - (beta * beta)).sqrt();

            self.to
                + envelope
                    * (x0 * (omega1 * t).cos() + ((beta * x0 + v0) / omega1) * (omega1 * t).sin())
        } else {
            // Overdamped.
            let omega2 = ((beta * beta) - (omega0 * omega0)).sqrt();

            self.to
                + envelope
                    * (x0 * (omega2 * t).cosh() + ((beta * x0 + v0) / omega2) * (omega2 * t).sinh())
        }
    }

    /// Returns the spring velocity at a given time in seconds.
    ///
    /// This is the analytical derivative of [`oscillate()`](Self::oscillate).
    fn velocity(&self, t: f64) -> f64 {
        let b = self.params.damping;
        let m = self.params.mass;
        let k = self.params.stiffness;
        let v0 = self.initial_velocity;

        let beta = b / (2. * m);
        let omega0 = (k / m).sqrt();

        let x0 = self.from - self.to;

        let envelope = (-beta * t).exp();

        // Derivatives of the solutions in oscillate().
        if (beta - omega0).abs() <= f64::from(f32::EPSILON) {
            // Critically damped: x = to + e^(-βt) * (x0 + (βx0 + v0) t).
            let c = beta * x0 + v0;
            envelope * (c - beta * (x0 + c * t))
        } else if beta < omega0 {
            // Underdamped: x = to + e^(-βt) * (x0 cos(ωt) + c sin(ωt)).
            let omega1 = ((omega0 * omega0) - (beta * beta)).sqrt();
            let c = (beta * x0 + v0) / omega1;
            envelope
                * (omega1 * (c * (omega1 * t).cos() - x0 * (omega1 * t).sin())
                    - beta * (x0 * (omega1 * t).cos() + c * (omega1 * t).sin()))
        } else {
            // Overdamped: x = to + e^(-βt) * (x0 cosh(ωt) + c sinh(ωt)).
            let omega2 = ((beta * beta) - (omega0 * omega0)).sqrt();
            let c = (beta * x0 + v0) / omega2;
            envelope
                * (omega2 * (x0 * (omega2 * t).sinh() + c * (omega2 * t).cosh())
                    - beta * (x0 * (omega2 * t).cosh() + c * (omega2 * t).sinh()))
        }
    }

    /// Returns the spring velocity at a given time.
    pub fn velocity_at(&self, t: Duration) -> f64 {
        self.velocity(t.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overdamped_spring_equal_from_to_nan() {
        let spring = Spring {
            from: 0.,
            to: 0.,
            initial_velocity: 0.,
            params: SpringParams::new(1.15, 850., 0.0001),
        };
        let _ = spring.duration();
        let _ = spring.clamped_duration();
        let _ = spring.value_at(Duration::ZERO);
    }

    #[test]
    fn overdamped_spring_duration_panic() {
        let spring = Spring {
            from: 0.,
            to: 1.,
            initial_velocity: 0.,
            params: SpringParams::new(6., 1200., 0.0001),
        };
        let _ = spring.duration();
        let _ = spring.clamped_duration();
        let _ = spring.value_at(Duration::ZERO);
    }

    /// Springs covering each damping regime.
    fn springs() -> [Spring; 3] {
        [
            // Underdamped.
            Spring {
                from: 0.,
                to: 1.,
                initial_velocity: 0.5,
                params: SpringParams::new(0.6, 800., 0.0001),
            },
            // Critically damped.
            Spring {
                from: 0.,
                to: 1.,
                initial_velocity: -0.5,
                params: SpringParams::new(1., 800., 0.0001),
            },
            // Overdamped.
            Spring {
                from: 1.,
                to: 0.,
                initial_velocity: 0.5,
                params: SpringParams::new(1.5, 800., 0.0001),
            },
        ]
    }

    #[test]
    fn velocity_at_start_is_initial_velocity() {
        for spring in springs() {
            let v = spring.velocity_at(Duration::ZERO);
            assert!(
                (v - spring.initial_velocity).abs() < 1e-9,
                "velocity at t=0 must equal initial_velocity: {v} vs {}",
                spring.initial_velocity
            );
        }
    }

    #[test]
    fn velocity_matches_finite_difference() {
        const DT: f64 = 1e-6;

        for spring in springs() {
            for ms in [10, 50, 100, 250] {
                let t = Duration::from_millis(ms);
                let analytical = spring.velocity_at(t);

                let numerical = (spring.value_at(t + Duration::from_secs_f64(DT))
                    - spring.value_at(t - Duration::from_secs_f64(DT)))
                    / (2. * DT);

                assert!(
                    (analytical - numerical).abs() < 1e-3,
                    "velocity mismatch at {ms}ms: analytical {analytical} vs numerical {numerical}"
                );
            }
        }
    }

    #[test]
    fn velocity_near_completion_is_zero() {
        for spring in springs() {
            let duration = spring.duration();
            if duration == Duration::MAX {
                continue;
            }

            // duration() is where the position settles within epsilon; the
            // velocity keeps decaying, so sample a second past it.
            let v = spring.velocity_at(duration + Duration::from_secs(1));
            assert!(
                v.abs() < 0.01,
                "velocity at rest must be near zero, got {v}"
            );
        }
    }

    #[test]
    fn velocity_sign_on_reversal() {
        // A spring moving away from its target has a negative velocity
        // relative to the from→to direction.
        let spring = Spring {
            from: 0.,
            to: 1.,
            initial_velocity: -2.,
            params: SpringParams::new(1., 800., 0.0001),
        };

        assert!(spring.velocity_at(Duration::ZERO) < 0.);
        // The spring eventually reverses and moves towards the target.
        assert!(spring.velocity_at(Duration::from_millis(200)) > 0.);
    }
}
