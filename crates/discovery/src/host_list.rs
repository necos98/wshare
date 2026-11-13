//! [`HostList`]: the viewer's live list of hosts (pure state).

use std::collections::HashMap;

use crate::{DiscoveredHost, DiscoveryEvent};

/// Live set of discovered hosts, keyed by host id.
#[derive(Debug, Default, Clone)]
pub struct HostList {
    hosts: HashMap<String, DiscoveredHost>,
}

impl HostList {
    /// Applies an event; returns `true` if the list changed (i.e. the UI should refresh).
    pub fn apply(&mut self, event: DiscoveryEvent) -> bool {
        match event {
            DiscoveryEvent::Found(host) => {
                let previous = self
                    .hosts
                    .insert(host.announcement.id.clone(), host.clone());
                previous.as_ref() != Some(&host)
            }
            DiscoveryEvent::Lost { id } => self.hosts.remove(&id).is_some(),
        }
    }

    /// Hosts sorted by display name (case-insensitive), ties broken by id.
    pub fn hosts(&self) -> Vec<&DiscoveredHost> {
        let mut hosts: Vec<&DiscoveredHost> = self.hosts.values().collect();
        hosts.sort_by_cached_key(|h| {
            (
                h.announcement.display_name.to_lowercase(),
                h.announcement.id.clone(),
            )
        });
        hosts
    }

    /// Looks up a host by id.
    pub fn get(&self, id: &str) -> Option<&DiscoveredHost> {
        self.hosts.get(id)
    }

    /// Number of hosts.
    pub fn len(&self) -> usize {
        self.hosts.len()
    }

    /// Whether no host is known.
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::txt::sample;

    fn found(id: &str, name: &str, ip: &str) -> DiscoveryEvent {
        DiscoveryEvent::Found(DiscoveredHost::new(
            sample(id, name),
            vec![ip.parse().unwrap()],
        ))
    }

    fn lost(id: &str) -> DiscoveryEvent {
        DiscoveryEvent::Lost { id: id.into() }
    }

    fn names(list: &HostList) -> Vec<(&str, &str)> {
        list.hosts()
            .iter()
            .map(|h| (h.announcement.display_name.as_str(), h.id()))
            .collect()
    }

    #[test]
    fn add_update_remove() {
        let mut list = HostList::default();
        assert!(list.is_empty());
        assert!(list.apply(found("a", "Alpha", "10.0.0.1")));
        assert!(
            !list.apply(found("a", "Alpha", "10.0.0.1")),
            "same data: no change"
        );
        assert!(
            list.apply(found("a", "Alpha", "10.0.0.2")),
            "new address: change"
        );
        assert_eq!(list.len(), 1);
        assert_eq!(
            list.get("a").unwrap().addresses,
            ["10.0.0.2".parse::<std::net::IpAddr>().unwrap()]
        );
        assert!(list.apply(lost("a")));
        assert!(!list.apply(lost("a")), "unknown id: no change");
        assert!(list.get("a").is_none());
    }

    #[test]
    fn sorted_by_name_then_id() {
        let mut list = HostList::default();
        list.apply(found("z", "beta", "10.0.0.1"));
        list.apply(found("b", "Alpha", "10.0.0.2"));
        list.apply(found("a", "Alpha", "10.0.0.3"));
        list.apply(found("c", "Gamma", "10.0.0.4"));
        assert_eq!(
            names(&list),
            [
                ("Alpha", "a"),
                ("Alpha", "b"),
                ("beta", "z"),
                ("Gamma", "c")
            ]
        );
        // Renaming a host re-sorts it.
        list.apply(found("c", "Aardvark", "10.0.0.4"));
        assert_eq!(names(&list)[0], ("Aardvark", "c"));
    }
}
