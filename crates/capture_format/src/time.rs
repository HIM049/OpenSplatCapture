/// 同一时刻采样的单调时钟与 Unix 时间，用来把各路时间戳换算到 Unix 毫秒。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeBase {
    pub mono_ns: i64,
    pub unix_ms: i64,
}

impl TimeBase {
    pub fn unix_ms(&self, mono_ns: i64) -> i64 {
        self.unix_ms + (mono_ns - self.mono_ns).div_euclid(1_000_000)
    }

    pub fn unix_ms_f64(&self, mono_ns: i64) -> f64 {
        self.unix_ms as f64 + (mono_ns - self.mono_ns) as f64 / 1e6
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_monotonic_to_unix() {
        let base = TimeBase {
            mono_ns: 5_000_000_000,
            unix_ms: 1_700_000_000_000,
        };
        assert_eq!(base.unix_ms(5_000_000_000), 1_700_000_000_000);
        assert_eq!(base.unix_ms(5_123_456_789), 1_700_000_000_123);
        assert_eq!(base.unix_ms(4_999_000_000), 1_699_999_999_999);
        assert!((base.unix_ms_f64(5_000_500_000) - 1_700_000_000_000.5).abs() < 1e-6);
    }
}
