//! `FaultForge` operator CLI (`faultforge`) — skeleton.
//!
//! Future home of the `faultforge agents list/show/rename/rm` subcommands,
//! which talk to the master's REST API. For now this is a placeholder
//! entrypoint that compiles.

fn banner() -> &'static str {
    "faultforge (skeleton) — break it before it breaks you"
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::banner;

    #[test]
    fn banner_is_set() {
        assert!(banner().contains("faultforge"));
    }
}
