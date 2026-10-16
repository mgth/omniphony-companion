//! Physical volume matching policy, independent of acquisition and digital profiles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    pub microphone_dbfs: f64,
    /// Band power after subtracting background and removing existing digital trim.
    /// A relative comparison reading, not calibrated SPL.
    pub comparison_db: f64,
    pub snr_db: f64,
}

pub fn estimate(power: f64, background: f64, digital_trim_db: f64) -> Option<Level> {
    if !power.is_finite()
        || !background.is_finite()
        || !digital_trim_db.is_finite()
        || power <= 0.0
        || background <= 0.0
        || power <= background
    {
        return None;
    }
    let signal = power - background;
    Some(Level {
        microphone_dbfs: 10.0 * power.log10(),
        comparison_db: 10.0 * signal.log10() - digital_trim_db,
        snr_db: 10.0 * (signal / background).log10(),
    })
}
impl Level {
    pub fn usable(&self) -> bool {
        self.microphone_dbfs.is_finite()
            && self.comparison_db.is_finite()
            && self.snr_db.is_finite()
            && self.snr_db >= 10.0
    }
    pub fn difference(&self, reference: &Self) -> Option<f64> {
        (self.usable() && reference.usable())
            .then_some(self.comparison_db - reference.comparison_db)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comparison_removes_background_and_existing_trims_without_a_gain_proposal() {
        let a = estimate(0.0101, 0.0001, 0.0).unwrap();
        let b = estimate(0.0401, 0.0001, 6.020599913).unwrap();
        assert!(b.difference(&a).unwrap().abs() < 1e-8);
        assert!(
            (estimate(0.0201, 0.0001, 0.0)
                .unwrap()
                .difference(&a)
                .unwrap()
                - 3.010299957)
                .abs()
                < 1e-8
        );
        assert!(
            estimate(0.0002, 0.0001, 0.0)
                .unwrap()
                .difference(&a)
                .is_none()
        );
        for p in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            assert!(estimate(p, 0.0001, 0.0).is_none());
        }
        assert!(estimate(0.1, 0.0, 0.0).is_none());
    }
}
