use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use faultforge_proto::Hostname;

use crate::lock::lock_poison_free;

#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub hostname: Hostname,
    pub name: String,
    pub last_seen: SystemTime,
    /// Host quarantine per the agent's last `TaintStatus` (`false` until any
    /// report). Updated only from agent frames — the master never assumes it.
    pub tainted: bool,
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
    let mut reg = lock_poison_free(registry);
    // Re-registration must not read as "clean" before the agent's own
    // TaintStatus arrives moments later — carry the last known quarantine over.
    let tainted = reg.get(hostname).is_some_and(|info| info.tainted);
    let info = AgentInfo {
        name: hostname.to_string(),
        hostname: hostname.clone(),
        last_seen: now,
        tainted,
    };
    reg.insert(hostname.clone(), info);
}

/// Record the host quarantine state reported by the agent's `TaintStatus`.
///
/// Does nothing if the hostname is not found in the registry.
///
/// # Panics
///
/// Panics if the registry mutex is poisoned.
pub fn set_taint(registry: &Registry, hostname: &Hostname, tainted: bool) {
    if let Some(entry) = lock_poison_free(registry).get_mut(hostname) {
        entry.tainted = tainted;
    }
}

/// Update the `last_seen` timestamp for a registered agent.
///
/// Does nothing if the hostname is not found in the registry.
///
/// # Panics
///
/// Panics if the registry mutex is poisoned.
pub fn update_heartbeat(registry: &Registry, hostname: &Hostname, now: SystemTime) {
    if let Some(entry) = lock_poison_free(registry).get_mut(hostname) {
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

    #[test]
    fn taint_is_recorded_and_survives_re_registration() {
        let registry = new_registry();
        let t = UNIX_EPOCH + Duration::from_secs(1000);
        register_agent(&registry, &hostname("web-01"), t);
        assert!(!registry.lock().unwrap()[&hostname("web-01")].tainted);

        set_taint(&registry, &hostname("web-01"), true);
        assert!(registry.lock().unwrap()[&hostname("web-01")].tainted);

        register_agent(&registry, &hostname("web-01"), t);
        assert!(
            registry.lock().unwrap()[&hostname("web-01")].tainted,
            "re-registration must not clear the quarantine flag"
        );

        set_taint(&registry, &hostname("web-01"), false);
        assert!(!registry.lock().unwrap()[&hostname("web-01")].tainted);
    }
}
