use std::time::Duration;

use crate::config::schema::ResponseTimeConfig;

/// Returns `true` when `X-Response-Time` should be added for this site.
pub fn is_enabled(cfg: Option<&ResponseTimeConfig>) -> bool {
    match cfg {
        None | Some(ResponseTimeConfig::Enabled(false)) => false,
        Some(ResponseTimeConfig::Enabled(true)) | Some(ResponseTimeConfig::Options(_)) => true,
    }
}

/// Number of decimal digits after the millisecond point (0 → integer ms, e.g. `"12ms"`).
pub fn decimal_digits(cfg: Option<&ResponseTimeConfig>) -> u8 {
    match cfg {
        Some(ResponseTimeConfig::Options(opts)) => opts.digits.unwrap_or(0),
        _ => 0,
    }
}

/// Format `elapsed` as e.g. `"12ms"` or `"12.345ms"`.
pub fn format_elapsed(elapsed: Duration, digits: u8) -> String {
    let ms = elapsed.as_secs_f64() * 1_000.0;
    if digits == 0 {
        format!("{ms:.0}ms")
    } else {
        format!("{ms:.prec$}ms", prec = digits as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{ResponseTimeConfig, ResponseTimeOptions};

    #[test]
    fn disabled_when_none() {
        assert!(!is_enabled(None));
    }

    #[test]
    fn disabled_when_false() {
        assert!(!is_enabled(Some(&ResponseTimeConfig::Enabled(false))));
    }

    #[test]
    fn enabled_when_true() {
        assert!(is_enabled(Some(&ResponseTimeConfig::Enabled(true))));
    }

    #[test]
    fn enabled_when_options() {
        let cfg = ResponseTimeConfig::Options(ResponseTimeOptions { digits: Some(2) });
        assert!(is_enabled(Some(&cfg)));
    }

    #[test]
    fn format_integer_ms() {
        let s = format_elapsed(Duration::from_millis(42), 0);
        assert_eq!(s, "42ms");
    }

    #[test]
    fn format_decimal_ms() {
        let s = format_elapsed(Duration::from_micros(12_345), 2);
        // 12345 µs = 12.345 ms → "12.35ms" after rounding to 2 dp
        assert!(s.starts_with("12.3"), "got {s}");
        assert!(s.ends_with("ms"), "got {s}");
    }

    #[test]
    fn format_zero_duration() {
        let s = format_elapsed(Duration::ZERO, 0);
        assert_eq!(s, "0ms", "zero duration should be 0ms");
    }

    #[test]
    fn format_large_duration() {
        let s = format_elapsed(Duration::from_secs(2), 0);
        assert_eq!(s, "2000ms", "2s should format as 2000ms");
    }

    #[test]
    fn decimal_digits_returns_zero_for_none() {
        assert_eq!(decimal_digits(None), 0, "None config → 0 digits");
    }

    #[test]
    fn decimal_digits_returns_zero_for_enabled_true() {
        assert_eq!(
            decimal_digits(Some(&ResponseTimeConfig::Enabled(true))),
            0,
            "Enabled(true) → 0 digits"
        );
    }

    #[test]
    fn decimal_digits_options_none_defaults_to_zero() {
        let cfg = ResponseTimeConfig::Options(ResponseTimeOptions { digits: None });
        assert_eq!(
            decimal_digits(Some(&cfg)),
            0,
            "Options with digits: None → 0 digits"
        );
    }

    #[test]
    fn decimal_digits_options_with_value() {
        let cfg = ResponseTimeConfig::Options(ResponseTimeOptions { digits: Some(3) });
        assert_eq!(decimal_digits(Some(&cfg)), 3, "Options digits: 3 → 3");
    }
}
