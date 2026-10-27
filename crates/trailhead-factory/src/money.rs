use std::fmt;
use std::ops::{Add, AddAssign};

use serde::{Deserialize, Serialize};

/// US dollars in millionths, so spend sums exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Usd(pub u64);

impl Usd {
    pub const ZERO: Usd = Usd(0);

    pub fn micros(micros: u64) -> Usd {
        Usd(micros)
    }

    pub fn dollars(dollars: f64) -> Usd {
        Usd((dollars * 1_000_000.0).round().max(0.0) as u64)
    }

    pub fn as_dollars(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }
}

impl Add for Usd {
    type Output = Usd;
    fn add(self, rhs: Usd) -> Usd {
        Usd(self.0.saturating_add(rhs.0))
    }
}

impl AddAssign for Usd {
    fn add_assign(&mut self, rhs: Usd) {
        *self = *self + rhs;
    }
}

impl fmt::Display for Usd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${:.4}", self.as_dollars())
    }
}

/// Token counts as reported in a Messages response's `usage`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl AddAssign for Usage {
    fn add_assign(&mut self, rhs: Usage) {
        self.input_tokens += rhs.input_tokens;
        self.output_tokens += rhs.output_tokens;
        self.cache_read_tokens += rhs.cache_read_tokens;
        self.cache_write_tokens += rhs.cache_write_tokens;
    }
}
