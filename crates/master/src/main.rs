//! FaultForge master (control plane) — skeleton.
//!
//! Future home of the gRPC `AgentService` server, the axum REST API, the
//! in-memory connection registry, the SQLite-backed `AgentStore`, and the
//! staleness sweep. For now this is a placeholder entrypoint that compiles.

fn banner() -> &'static str {
    "faultforge-master (skeleton) — break it before it breaks you"
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::banner;

    #[test]
    fn banner_is_set() {
        assert!(banner().contains("faultforge-master"));
    }
}
