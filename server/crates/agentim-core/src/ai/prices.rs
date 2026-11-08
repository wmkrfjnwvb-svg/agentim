//! What a model call costs (docs/en/design/assistants.md): the price table shipped in
//! `builtin/prices.toml` (or the deployment's own file), in nano-dollars so
//! that even a cache read of a few tokens is counted.

use serde::Deserialize;

const BUILTIN: &str = include_str!("../../builtin/prices.toml");

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub from: Option<String>,
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub long_above: Option<u64>,
    #[serde(default)]
    pub long_input: Option<f64>,
    #[serde(default)]
    pub long_output: Option<f64>,
    #[serde(default)]
    pub long_cache_read: Option<f64>,
    #[serde(default)]
    pub long_cache_write: Option<f64>,
    pub as_of: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProviderRule {
    #[serde(default)]
    pub peak_hours_utc: Vec<[u32; 2]>,
    #[serde(default)]
    pub offpeak_factor: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Prices {
    #[serde(default)]
    pub provider: std::collections::BTreeMap<String, ProviderRule>,
    #[serde(default)]
    pub price: Vec<Price>,
}

/// Tokens of one call, as the provider reported them. `input` excludes
/// cache reads and writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    pub fn add(&mut self, o: Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
    }
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
    /// The whole prompt (what long-context tiers look at).
    pub fn prompt(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }
}

impl Prices {
    pub fn builtin() -> Prices {
        Prices::parse(BUILTIN).expect("builtin/prices.toml")
    }

    pub fn parse(text: &str) -> Result<Prices, String> {
        let p: Prices = toml::from_str(text).map_err(|e| e.to_string())?;
        for x in &p.price {
            if x.model.is_empty() || x.provider.is_empty() {
                return Err("a price needs provider and model".into());
            }
            let all = [x.input, x.output]
                .into_iter()
                .chain(x.cache_read)
                .chain(x.cache_write)
                .chain(x.long_input)
                .chain(x.long_output)
                .chain(x.long_cache_read)
                .chain(x.long_cache_write);
            for v in all {
                if !(v.is_finite() && v >= 0.0) {
                    return Err(format!("{}: prices must be 0 or more", x.model));
                }
            }
            if let Some(f) = &x.from
                && day_number(f).is_none()
            {
                return Err(format!("{}: from must be YYYY-MM-DD", x.model));
            }
        }
        for (name, r) in &p.provider {
            if r.peak_hours_utc.iter().any(|[a, b]| a >= b || *b > 24) {
                return Err(format!("{name}: peak hours are [start, end) within 0..24"));
            }
        }
        Ok(p)
    }

    /// The price in force at `now_ms` for a model: the longest matching
    /// model name, then the latest `from` not after now.
    pub fn find(&self, provider: &str, model: &str, now_ms: i64) -> Option<&Price> {
        let today = now_ms.div_euclid(86_400_000);
        let fits =
            |m: &str| model == m || model.strip_prefix(m).is_some_and(|r| r.starts_with('-'));
        let best = self
            .price
            .iter()
            .filter(|p| p.provider == provider && fits(&p.model))
            .map(|p| p.model.len())
            .max()?;
        self.price
            .iter()
            .filter(|p| p.provider == provider && fits(&p.model) && p.model.len() == best)
            .filter(|p| {
                p.from
                    .as_deref()
                    .and_then(day_number)
                    .is_none_or(|d| d <= today)
            })
            .max_by_key(|p| p.from.as_deref().and_then(day_number).unwrap_or(i64::MIN))
    }

    /// The cost in nano-dollars, or `None` when the model has no price.
    pub fn cost(&self, provider: &str, model: &str, u: &Usage, now_ms: i64) -> Option<i64> {
        let p = self.find(provider, model, now_ms)?;
        let long = p.long_above.is_some_and(|n| u.prompt() > n);
        let pick = |base: f64, l: Option<f64>| if long { l.unwrap_or(base) } else { base };
        let input = pick(p.input, p.long_input);
        let output = pick(p.output, p.long_output);
        let read = pick(p.cache_read.unwrap_or(p.input), p.long_cache_read);
        let write = pick(p.cache_write.unwrap_or(p.input), p.long_cache_write);
        // USD per 1M tokens x tokens = micro-dollars; x 1000 = nano-dollars.
        let micros = u.input as f64 * input
            + u.output as f64 * output
            + u.cache_read as f64 * read
            + u.cache_write as f64 * write;
        let factor = self.factor(provider, now_ms);
        Some((micros * factor * 1000.0).round() as i64)
    }

    fn factor(&self, provider: &str, now_ms: i64) -> f64 {
        let Some(r) = self.provider.get(provider) else {
            return 1.0;
        };
        let (Some(f), false) = (r.offpeak_factor, r.peak_hours_utc.is_empty()) else {
            return 1.0;
        };
        let days = now_ms.div_euclid(86_400_000);
        // 1970-01-01 was a Thursday: 0 = Monday.
        let weekday = (days + 3).rem_euclid(7);
        let hour = (now_ms.rem_euclid(86_400_000) / 3_600_000) as u32;
        let peak = weekday < 5
            && r.peak_hours_utc
                .iter()
                .any(|[a, b]| hour >= *a && hour < *b);
        if peak { 1.0 } else { f }
    }
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date.
fn day_number(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let (y, m, d) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;
    /// 2026-11-06 (a Friday) 12:00 UTC.
    const NOW: i64 = 20_763 * DAY + 12 * 3_600_000;

    #[test]
    fn the_shipped_table_parses_and_prices_known_models() {
        let p = Prices::builtin();
        assert_eq!(day_number("2026-11-06"), Some(20_763));
        assert_eq!(day_number("1970-01-01"), Some(0));
        for (prov, m) in [
            ("anthropic", "claude-opus-5-5"),
            ("anthropic", "claude-sonnet-5-5"),
            ("anthropic", "claude-haiku-5-5"),
            ("openai", "gpt-5.3-codex"),
            ("openai", "gpt-5.4-mini"),
            ("gemini", "gemini-3.8-flash"),
            ("deepseek", "deepseek-flash"),
        ] {
            assert!(p.find(prov, m, NOW).is_some(), "{prov} {m}");
        }
        assert!(p.find("anthropic", "claude-unknown", NOW).is_none());
        assert!(
            p.find("openai", "claude-opus-5-5", NOW).is_none(),
            "providers are separate"
        );
    }

    #[test]
    fn costs() {
        let p = Prices::builtin();
        let u = Usage {
            input: 3000,
            output: 400,
            cache_read: 0,
            cache_write: 0,
        };
        // Sonnet 5.5: 3000 x $2 + 400 x $10 per 1M = $0.010.
        assert_eq!(
            p.cost("anthropic", "claude-sonnet-5-5", &u, NOW),
            Some(10_000_000)
        );
        // A dated variant uses the same price; "-" is needed after the name.
        assert_eq!(
            p.cost("anthropic", "claude-sonnet-5-5-20261001", &u, NOW),
            Some(10_000_000)
        );
        assert_eq!(p.cost("anthropic", "claude-sonnet-5-55", &u, NOW), None);
        // Cache reads and writes at their own prices.
        let c = Usage {
            input: 0,
            output: 0,
            cache_read: 1_000_000,
            cache_write: 1_000_000,
        };
        assert_eq!(
            p.cost("anthropic", "claude-sonnet-5-5", &c, NOW),
            Some(2_600_000_000)
        );
        // Haiku above 100K prompt tokens: the long prices for everything.
        let long = Usage {
            input: 100_001,
            output: 1000,
            cache_read: 0,
            cache_write: 0,
        };
        assert_eq!(
            p.cost("anthropic", "claude-haiku-5-5", &long, NOW),
            Some((100_001.0f64 * 0.5 * 1000.0 + 1000.0 * 2.5 * 1000.0).round() as i64)
        );
        let at = Usage {
            input: 100_000,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        };
        assert_eq!(
            p.cost("anthropic", "claude-haiku-5-5", &at, NOW),
            Some(10_000_000)
        );
        // Zero tokens cost nothing.
        assert_eq!(p.cost("openai", "gpt-5.4", &Usage::default(), NOW), Some(0));
    }

    #[test]
    fn prices_change_on_their_day() {
        let p = Prices::builtin();
        let u = Usage {
            input: 1_000_000,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        };
        let dec31 = day_number("2026-12-31").unwrap() * DAY + 23 * 3_600_000;
        let jan1 = day_number("2027-01-01").unwrap() * DAY;
        assert_eq!(
            p.cost("gemini", "gemini-3.8-flash", &u, dec31),
            Some(750_000_000)
        );
        assert_eq!(
            p.cost("gemini", "gemini-3.8-flash", &u, jan1),
            Some(1_500_000_000)
        );
    }

    #[test]
    fn deepseek_off_peak_is_half() {
        let p = Prices::builtin();
        let u = Usage {
            input: 1_000_000,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        };
        let fri = 20_763 * DAY;
        assert_eq!(
            p.cost("deepseek", "deepseek-flash", &u, fri + 2 * 3_600_000),
            Some(300_000_000),
            "peak"
        );
        assert_eq!(
            p.cost("deepseek", "deepseek-flash", &u, fri + 4 * 3_600_000),
            Some(150_000_000),
            "04:00 is off-peak"
        );
        assert_eq!(
            p.cost(
                "deepseek",
                "deepseek-flash",
                &u,
                fri + 9 * 3_600_000 + 3_599_999
            ),
            Some(300_000_000)
        );
        // Saturday at a peak hour is off-peak.
        assert_eq!(
            p.cost("deepseek", "deepseek-flash", &u, fri + DAY + 2 * 3_600_000),
            Some(150_000_000)
        );
        // Other providers have no schedule.
        assert_eq!(
            p.cost("openai", "gpt-5.4", &u, fri + DAY),
            Some(2_500_000_000)
        );
    }

    #[test]
    fn bad_tables_are_refused() {
        assert!(
            Prices::parse(
                "[[price]]\nprovider = \"a\"\nmodel = \"m\"\ninput = -1\noutput = 1\nas_of = \"x\""
            )
            .is_err()
        );
        assert!(Prices::parse("[[price]]\nprovider = \"a\"\nmodel = \"m\"\ninput = 1\noutput = 1\nfrom = \"2026-13-01\"\nas_of = \"x\"").is_err());
        assert!(Prices::parse("[provider.a]\npeak_hours_utc = [[5, 3]]").is_err());
        assert!(Prices::parse("[[price]]\nprovider = \"a\"\nmodel = \"m\"\ninput = 1\noutput = 1\nas_of = \"x\"\nextra = 1").is_err());
        assert_eq!(Prices::parse("").unwrap(), Prices::default());
    }
}
