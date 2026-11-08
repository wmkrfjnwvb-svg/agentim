//! Tenants (docs/en/design/multi-tenancy.md): one hosted project per tenant, each in its own
//! database and its own instance of this server. Inside an instance
//! everything belongs to `default`, exactly as on a self-hosted server.

/// The tenant of a self-hosted server and of everything inside a tenant's
/// own instance.
pub const DEFAULT: &str = crate::model::TENANT_DEFAULT;

/// Longest tenant id (it names a database and a host).
pub const MAX_LEN: usize = 15;

/// A tenant id: 1–15 of `a-z`, `0-9`, `-`, not starting or ending with `-`.
pub fn valid(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= MAX_LEN
        && !t.starts_with('-')
        && !t.ends_with('-')
        && t.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        for ok in ["acme", "p0123456789abcd", "a-b", "x"] {
            assert!(valid(ok), "{ok}");
        }
        for bad in [
            "",
            "-a",
            "a-",
            "Acme",
            "a_b",
            "a.b",
            "p0123456789abcde",
            "a b",
        ] {
            assert!(!valid(bad), "{bad}");
        }
    }
}
