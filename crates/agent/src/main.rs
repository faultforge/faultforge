//! FaultForge agent — skeleton.
//!
//! Future home of the local stable-id state, the gRPC client that dials the
//! master, the register/heartbeat loop, and reconnect-with-backoff. For now
//! this is a placeholder entrypoint that compiles.

fn banner() -> &'static str {
    "faultforge-agent (skeleton) — break it before it breaks you"
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::banner;

    #[test]
    fn banner_is_set() {
        assert!(banner().contains("faultforge-agent"));
    }
}
