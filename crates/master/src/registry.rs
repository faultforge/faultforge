use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use faultforge_proto::Hostname;

#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub hostname: Hostname,
    pub name: String,
    pub last_seen: SystemTime,
}

pub type Registry = Arc<Mutex<HashMap<Hostname, AgentInfo>>>;

#[must_use]
pub fn new_registry() -> Registry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Register an agent in the registry, replacing any existing entry for the hostname.
///
/// # Panics
///
/// Panics if the registry mutex is poisoned.
pub fn register_agent(registry: &Registry, hostname: &Hostname, now: SystemTime) {
    let info = AgentInfo {
        name: hostname.to_string(),
        hostname: hostname.clone(),
        last_seen: now,
    };
    #[allow(clippy::expect_used)]
    // mutex poison means a previous thread panicked; propagating is correct
    registry
        .lock()
        .expect("registry lock poisoned")
        .insert(hostname.clone(), info);
}

/// Update the `last_seen` timestamp for a registered agent.
///
/// Does nothing if the hostname is not found in the registry.
///
/// # Panics
///
/// Panics if the registry mutex is poisoned.
pub fn update_heartbeat(registry: &Registry, hostname: &Hostname, now: SystemTime) {
    #[allow(clippy::expect_used)]
    // mutex poison means a previous thread panicked; propagating is correct
    if let Some(entry) = registry
        .lock()
        .expect("registry lock poisoned")
        .get_mut(hostname)
    {
        entry.last_seen = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faultforge_proto::Hostname;
    use std::time::{Duration, UNIX_EPOCH};

    fn hostname(s: &str) -> Hostname {
        Hostname::parse(s).unwrap()
    }

    #[test]
    fn register_seeds_name_from_hostname() {
        let registry = new_registry();
        let t = UNIX_EPOCH + Duration::from_secs(1000);
        register_agent(&registry, &hostname("web-01"), t);
        let reg = registry.lock().unwrap();
        let entry = reg.get(&hostname("web-01")).unwrap();
        assert_eq!(entry.name, "web-01");
        assert_eq!(entry.last_seen, t);
    }

    #[test]
    fn second_register_supersedes_first() {
        let registry = new_registry();
        let t1 = UNIX_EPOCH + Duration::from_secs(1000);
        let t2 = UNIX_EPOCH + Duration::from_secs(2000);
        register_agent(&registry, &hostname("web-01"), t1);
        register_agent(&registry, &hostname("web-01"), t2);
        let reg = registry.lock().unwrap();
        assert_eq!(reg.len(), 1);
        assert_eq!(reg[&hostname("web-01")].last_seen, t2);
    }

    #[test]
    fn heartbeat_updates_last_seen() {
        let registry = new_registry();
        let t1 = UNIX_EPOCH + Duration::from_secs(1000);
        let t2 = UNIX_EPOCH + Duration::from_secs(2000);
        register_agent(&registry, &hostname("web-01"), t1);
        update_heartbeat(&registry, &hostname("web-01"), t2);
        assert_eq!(registry.lock().unwrap()[&hostname("web-01")].last_seen, t2);
    }
}
