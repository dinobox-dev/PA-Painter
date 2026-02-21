use glam::Vec2;
use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

/// Deterministic PRNG wrapper. Same seed always produces same sequence.
pub struct SeededRng {
    rng: ChaCha8Rng,
}

impl SeededRng {
    pub fn new(seed: u32) -> Self {
        Self {
            rng: ChaCha8Rng::seed_from_u64(seed as u64),
        }
    }

    /// Random f32 in [0, 1).
    pub fn next_f32(&mut self) -> f32 {
        self.rng.gen::<f32>()
    }

    /// Random f32 in [min, max).
    pub fn next_f32_range(&mut self, min: f32, max: f32) -> f32 {
        min + self.next_f32() * (max - min)
    }

    /// Random i32 in [min, max] (inclusive).
    pub fn next_i32_range(&mut self, min: i32, max: i32) -> i32 {
        self.rng.gen_range(min..=max)
    }

    /// Random point in circle of given radius.
    pub fn random_in_circle(&mut self, radius: f32) -> Vec2 {
        let angle = self.next_f32() * std::f32::consts::TAU;
        let r = self.next_f32().sqrt() * radius;
        Vec2::new(r * angle.cos(), r * angle.sin())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut rng1 = SeededRng::new(42);
        let mut rng2 = SeededRng::new(42);
        let seq1: Vec<f32> = (0..10).map(|_| rng1.next_f32()).collect();
        let seq2: Vec<f32> = (0..10).map(|_| rng2.next_f32()).collect();
        assert_eq!(seq1, seq2);
    }

    #[test]
    fn different_seed_different_sequence() {
        let mut rng1 = SeededRng::new(42);
        let mut rng2 = SeededRng::new(43);
        let seq1: Vec<f32> = (0..10).map(|_| rng1.next_f32()).collect();
        let seq2: Vec<f32> = (0..10).map(|_| rng2.next_f32()).collect();
        assert_ne!(seq1, seq2);
    }

    #[test]
    fn random_in_circle_within_radius() {
        let mut rng = SeededRng::new(99);
        for _ in 0..100 {
            let p = rng.random_in_circle(1.0);
            assert!(p.length() <= 1.0 + 1e-6);
        }
    }

    #[test]
    fn f32_range_bounds() {
        let mut rng = SeededRng::new(7);
        for _ in 0..100 {
            let v = rng.next_f32_range(2.0, 5.0);
            assert!(v >= 2.0 && v < 5.0);
        }
    }

    #[test]
    fn i32_range_bounds() {
        let mut rng = SeededRng::new(7);
        for _ in 0..100 {
            let v = rng.next_i32_range(-3, 3);
            assert!(v >= -3 && v <= 3);
        }
    }
}
