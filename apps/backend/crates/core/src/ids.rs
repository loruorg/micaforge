//! Primary keys.
//!
//! Every Postgres row in `docs/CONTRACT.md` §2 keys on a UUID generated here
//! rather than by the database. Two reasons: the application can build a whole
//! object graph (org, site, membership) before it opens a transaction, and the
//! id is available for logging and for the response even if the insert later
//! fails.

/// A fresh UUIDv7.
///
/// Version 7 puts a millisecond timestamp in the high bits, so keys sort by
/// creation time. That keeps B-tree inserts at the right-hand edge of the index
/// instead of scattering them the way v4 does, and it means `ORDER BY id` is a
/// usable proxy for `ORDER BY created_at` without a second index.
///
/// This is a key, not a secret. Anything that has to be unguessable (a share
/// slug, an API key, a session token) comes from
/// [`crypto::random_token`](crate::crypto::random_token) instead.
pub fn new_id() -> uuid::Uuid {
    uuid::Uuid::now_v7()
}

#[cfg(test)]
mod tests {
    use super::new_id;

    #[test]
    fn ids_are_v7_and_ascend() {
        let a = new_id();
        let b = new_id();
        assert_eq!(a.get_version_num(), 7);
        assert_ne!(a, b);
        // Same millisecond or later: v7 never sorts backwards.
        assert!(b >= a || b.as_bytes()[..6] >= a.as_bytes()[..6]);
    }
}
