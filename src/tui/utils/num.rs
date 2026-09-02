const UNITS: [&str; 4] = ["k", "M", "B", "T"];

/// Display string for a token count: exact below 1,000, then abbreviated with
/// one decimal (`1.1k`, `12.3k`) and none past three digits (`128k`, `1.2M`).
pub fn fmt_tokens(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    let mut scaled = n as f64 / 1_000.0;
    let mut unit = 0;
    while scaled >= 999.5 && unit + 1 < UNITS.len() {
        scaled /= 1_000.0;
        unit += 1;
    }
    let number = if scaled >= 99.95 {
        format!("{scaled:.0}")
    } else {
        let s = format!("{scaled:.1}");
        match s.strip_suffix(".0") {
            Some(stripped) => stripped.to_string(),
            None => s,
        }
    };
    format!("{number}{}", UNITS[unit])
}

/// Display string for an estimated cost: `$0.12`, `$12.50`.
pub fn fmt_cost(cost: f64) -> String {
    format!("${cost:.2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_exact_below_one_thousand() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
    }

    #[test]
    fn tokens_abbreviated_k() {
        assert_eq!(fmt_tokens(1_000), "1k");
        assert_eq!(fmt_tokens(1_100), "1.1k");
        assert_eq!(fmt_tokens(9_999), "10k");
        assert_eq!(fmt_tokens(12_340), "12.3k");
        assert_eq!(fmt_tokens(128_000), "128k");
        assert_eq!(fmt_tokens(200_000), "200k");
    }

    #[test]
    fn tokens_abbreviated_m_and_b() {
        assert_eq!(fmt_tokens(999_999), "1M");
        assert_eq!(fmt_tokens(1_234_567), "1.2M");
        assert_eq!(fmt_tokens(200_000_000), "200M");
        assert_eq!(fmt_tokens(1_000_000_000), "1B");
    }

    #[test]
    fn cost_two_decimals() {
        assert_eq!(fmt_cost(0.0), "$0.00");
        assert_eq!(fmt_cost(0.125), "$0.12");
        assert_eq!(fmt_cost(12.5), "$12.50");
    }
}
