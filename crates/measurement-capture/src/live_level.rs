//! Bounded, one-second excited-band power meter. Runs on the service worker.
use crate::noise::Biquad;
use omniphony_osc_contract::measurement::LevelNoise;

pub struct Meter {
    filters: [Biquad; 4],
    powers: Vec<f64>,
    cursor: usize,
    count: usize,
    sum: f64,
    pub clipped: bool,
    pub invalid: bool,
}
impl Meter {
    pub fn new(rate: u32) -> Result<Self, &'static str> {
        LevelNoise {
            duration_seconds: 5,
            peak_dbfs: -30.0,
            bypass_output_gain: false,
        }
        .validate(rate)?;
        let rate_f = f64::from(rate);
        let low = f64::from(LevelNoise::LOW_HZ);
        let high = f64::from(LevelNoise::HIGH_HZ);
        Ok(Self {
            filters: [
                Biquad::new(low, rate_f, 0.541196100146197, true),
                Biquad::new(low, rate_f, 1.306562964876377, true),
                Biquad::new(high, rate_f, 0.541196100146197, false),
                Biquad::new(high, rate_f, 1.306562964876377, false),
            ],
            powers: vec![0.0; rate as usize],
            cursor: 0,
            count: 0,
            sum: 0.0,
            clipped: false,
            invalid: false,
        })
    }
    pub fn push(&mut self, sample: f32) {
        if !sample.is_finite() {
            self.invalid = true;
            return;
        }
        self.clipped |= sample.abs() >= 0.999;
        let value = self
            .filters
            .iter_mut()
            .fold(f64::from(sample), |x, f| f.process(x));
        let power = value * value;
        self.sum += power - self.powers[self.cursor];
        self.powers[self.cursor] = power;
        self.cursor = (self.cursor + 1) % self.powers.len();
        self.count = (self.count + 1).min(self.powers.len());
    }
    pub fn ready(&self) -> bool {
        self.count == self.powers.len()
    }
    pub fn power(&self) -> Option<f64> {
        (self.count > 0 && !self.invalid && !self.clipped)
            .then(|| (self.sum / self.count as f64).max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sine(meter: &mut Meter, hz: f64, amplitude: f64) {
        for i in 0..96000 {
            meter
                .push((amplitude * (std::f64::consts::TAU * hz * i as f64 / 48000.0).sin()) as f32);
        }
    }
    #[test]
    fn tracks_relative_level_rejects_rumble_and_remains_bounded() {
        let mut a = Meter::new(48000).unwrap();
        let mut b = Meter::new(48000).unwrap();
        sine(&mut a, 1000.0, 0.1);
        sine(&mut b, 1000.0, 0.2);
        assert!(a.ready());
        assert!((10.0 * (b.power().unwrap() / a.power().unwrap()).log10() - 6.0206).abs() < 0.001);
        let mut rumble = Meter::new(48000).unwrap();
        sine(&mut rumble, 50.0, 0.1);
        assert!(rumble.power().unwrap() < a.power().unwrap() * 1e-6);
        assert_eq!(a.powers.len(), 48000);
        a.push(f32::NAN);
        assert!(a.power().is_none());
        b.push(1.0);
        assert!(b.power().is_none());
        assert!(Meter::new(0).is_err());
    }
}
