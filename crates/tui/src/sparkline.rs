//! Throughput sparkline — a rolling window of throughput samples.

#[derive(Debug, Clone)]
pub struct Sparkline {
    samples: Vec<f64>,
    capacity: usize,
}

impl Sparkline {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: vec![0.0; capacity],
            capacity,
        }
    }

    pub fn push(&mut self, value: f64) {
        if self.samples.len() == self.capacity {
            self.samples.remove(0);
        }
        self.samples.push(value);
    }

    pub fn max(&self) -> f64 {
        self.samples.iter().cloned().fold(0.0, f64::max)
    }

    pub fn samples(&self) -> &[f64] {
        &self.samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_window_drops_oldest() {
        let mut s = Sparkline::new(3);
        s.push(1.0);
        s.push(2.0);
        s.push(3.0);
        s.push(4.0);
        assert_eq!(s.samples(), &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn max_returns_zero_when_empty() {
        let s = Sparkline::new(5);
        assert_eq!(s.max(), 0.0);
    }
}
