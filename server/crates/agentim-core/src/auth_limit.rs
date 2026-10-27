//! Failed authentications by source address (D130).
//!
//! A credential the server does not know is refused (401). Before refusing,
//! a token or key missing from memory is looked up in the database once, in
//! case it was created a moment ago by another node or process. Without a
//! limit per source, one address sending made-up tokens would use up that
//! lookup for everyone, and could guess for ever.
//!
//! - After [`MAX_FAILURES`] failures in [`WINDOW_MS`], an address is refused
//!   for [`BLOCK_MS`] (`rate_limited`, with the time to wait). GitLab bans
//!   an address for an hour after 10 failed authentications in a minute
//!   (<https://docs.gitlab.com/rate_limits/abuse_bans/>); we allow more and
//!   ban shorter, since offices and mobile carriers put many people behind
//!   one address.
//! - Each address may cause one database lookup per second for each kind
//!   of credential ([`Lookup`]), and all addresses together
//!   [`GLOBAL_LOOKUPS`] per second for each kind: an app that just created
//!   an API key and a signing key elsewhere has both found at once.
//!
//! A flood from many addresses is a DDoS for the load balancer or WAF in
//! front; here it costs at most [`GLOBAL_LOOKUPS`] reads a second.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

pub const MAX_FAILURES: u32 = 30;
pub const WINDOW_MS: i64 = 60_000;
pub const BLOCK_MS: i64 = 5 * 60_000;
pub const GLOBAL_LOOKUPS: u32 = 20;
/// Addresses remembered at most; beyond that the oldest entries go.
const MAX_ADDRS: usize = 100_000;

/// What is looked up: each has its own budget.
#[derive(Clone, Copy)]
pub enum Lookup {
    /// An agent key or client device token.
    Token = 0,
    /// A signing key (an unknown `kid` in a client JWT).
    SigningKey = 1,
    /// An open API key.
    ApiKey = 2,
}

#[derive(Default)]
struct Entry {
    /// Start of the current window and failures in it.
    window: i64,
    failures: u32,
    blocked_until: i64,
    last_lookup: [i64; 3],
}

#[derive(Default)]
pub struct AuthLimit {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    by_addr: HashMap<IpAddr, Entry>,
    /// Second and lookups in it, over all addresses.
    second: i64,
    lookups: [u32; 3],
}

/// The address of a caller whose address is not known (no peer, a
/// malformed proxy header): such callers share one budget.
pub fn key(ip: Option<IpAddr>) -> IpAddr {
    ip.unwrap_or(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED))
}

impl AuthLimit {
    /// `Err(wait_ms)` while the address is blocked.
    pub fn check(&self, ip: IpAddr, now: i64) -> Result<(), i64> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match g.by_addr.get(&ip) {
            Some(e) if e.blocked_until > now => Err(e.blocked_until - now),
            _ => Ok(()),
        }
    }

    /// One more failure; `Some(wait_ms)` when it blocks the address.
    pub fn failed(&self, ip: IpAddr, now: i64) -> Option<i64> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.by_addr.len() >= MAX_ADDRS && !g.by_addr.contains_key(&ip) {
            prune(&mut g.by_addr, now);
        }
        let e = g.by_addr.entry(ip).or_default();
        if now - e.window >= WINDOW_MS {
            e.window = now;
            e.failures = 0;
        }
        e.failures += 1;
        if e.failures >= MAX_FAILURES {
            e.blocked_until = now + BLOCK_MS;
            e.failures = 0;
            e.window = now;
            return Some(BLOCK_MS);
        }
        None
    }

    /// Whether this address may look a credential up in the database now.
    pub fn may_look_up(&self, ip: IpAddr, what: Lookup, now: i64) -> bool {
        let k = what as usize;
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let sec = now.div_euclid(1000);
        if g.second != sec {
            g.second = sec;
            g.lookups = [0; 3];
        }
        if g.lookups[k] >= GLOBAL_LOOKUPS {
            return false;
        }
        if g.by_addr.len() >= MAX_ADDRS && !g.by_addr.contains_key(&ip) {
            prune(&mut g.by_addr, now);
        }
        let e = g.by_addr.entry(ip).or_default();
        if e.last_lookup[k] != 0 && now - e.last_lookup[k] < 1000 {
            return false;
        }
        e.last_lookup[k] = now;
        g.lookups[k] += 1;
        true
    }
}

/// Drops entries that hold nothing any more, then (still full) the oldest
/// half: a flood of new addresses cannot grow memory without bound.
fn prune(m: &mut HashMap<IpAddr, Entry>, now: i64) {
    m.retain(|_, e| e.blocked_until > now || now - e.window < WINDOW_MS);
    if m.len() >= MAX_ADDRS {
        let mut ages: Vec<i64> = m
            .values()
            .map(|e| e.window.max(e.last_lookup.into_iter().max().unwrap_or(0)))
            .collect();
        ages.sort_unstable();
        let cut = ages[ages.len() / 2];
        m.retain(|_, e| {
            e.blocked_until > now
                || e.window.max(e.last_lookup.into_iter().max().unwrap_or(0)) > cut
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn failures_block_one_address_only() {
        let l = AuthLimit::default();
        for i in 0..MAX_FAILURES - 1 {
            assert_eq!(l.failed(ip(1), 1000 + i as i64), None);
        }
        assert_eq!(l.check(ip(1), 2000), Ok(()));
        assert_eq!(l.failed(ip(1), 2000), Some(BLOCK_MS));
        assert_eq!(l.check(ip(1), 2000), Err(BLOCK_MS));
        assert_eq!(l.check(ip(1), 2000 + BLOCK_MS - 1), Err(1));
        assert_eq!(l.check(ip(1), 2000 + BLOCK_MS), Ok(()));
        assert_eq!(l.check(ip(2), 2000), Ok(()), "others are not affected");
    }

    #[test]
    fn failures_spread_over_windows_do_not_block() {
        let l = AuthLimit::default();
        for i in 0..200 {
            // 29 a minute, for 200 minutes... one every ~2.07 s.
            assert_eq!(l.failed(ip(1), i * (WINDOW_MS / 29 + 1)), None, "{i}");
        }
        assert_eq!(l.check(ip(1), 200 * WINDOW_MS), Ok(()));
    }

    #[test]
    fn lookups_are_limited_per_address_and_in_all() {
        let l = AuthLimit::default();
        let t = Lookup::Token;
        assert!(l.may_look_up(ip(1), t, 10_000));
        assert!(!l.may_look_up(ip(1), t, 10_500), "one a second per address");
        assert!(
            l.may_look_up(ip(1), Lookup::ApiKey, 10_500),
            "each kind has its own"
        );
        assert!(l.may_look_up(ip(1), Lookup::SigningKey, 10_500));
        assert!(
            l.may_look_up(ip(2), t, 10_500),
            "another address has its own"
        );
        assert!(l.may_look_up(ip(1), t, 11_000));
        // All addresses together: GLOBAL_LOOKUPS a second.
        let mut n = 0;
        for i in 0..100 {
            if l.may_look_up(ip(10 + i), t, 20_000) {
                n += 1;
            }
        }
        assert_eq!(n, GLOBAL_LOOKUPS);
        assert!(l.may_look_up(ip(200), t, 21_000), "the next second");
        assert!(
            l.may_look_up(ip(201), Lookup::ApiKey, 20_000),
            "other kinds are not used up"
        );
    }

    #[test]
    fn memory_stays_bounded() {
        let l = AuthLimit::default();
        for i in 0..(MAX_ADDRS as u32 + 1000) {
            let a = IpAddr::from(i.to_be_bytes());
            l.failed(a, 1000 + (i / 1000) as i64);
        }
        assert!(l.inner.lock().unwrap().by_addr.len() <= MAX_ADDRS);
        // A blocked address survives pruning.
        for _ in 0..MAX_FAILURES {
            l.failed(ip(1), 5000);
        }
        for i in 0..(MAX_ADDRS as u32) {
            l.failed(IpAddr::from(((i + 1) << 8).to_be_bytes()), 6000);
        }
        assert!(l.check(ip(1), 6000).is_err());
    }
}
