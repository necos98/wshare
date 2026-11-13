//! [`DiscoveryEvent`] and the translation from `mdns-sd` events.

use std::collections::HashMap;

use mdns_sd::{ResolvedService, ServiceEvent};

use crate::names::id_from_fullname;
use crate::{DiscoveredHost, HostAnnouncement, SERVICE_TYPE, TxtError};

/// A change in the set of hosts visible on the LAN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    /// A host appeared or its announcement/addresses changed.
    Found(DiscoveredHost),
    /// A host withdrew its announcement (or its records expired).
    Lost {
        /// Id of the host that went away.
        id: String,
    },
}

/// Converts a resolved service into a host, validating its TXT record.
pub(crate) fn host_from_resolved(service: &ResolvedService) -> Result<DiscoveredHost, TxtError> {
    let txt: HashMap<String, String> = service
        .txt_properties
        .iter()
        .map(|p| (p.key().to_owned(), p.val_str().to_owned()))
        .collect();
    let announcement = HostAnnouncement::from_txt(service.port, &txt)?;
    let addresses = service.addresses.iter().map(|a| a.to_ip_addr()).collect();
    Ok(DiscoveredHost::new(announcement, addresses))
}

/// Stateful, network-free translator from `mdns-sd` [`ServiceEvent`]s to [`DiscoveryEvent`]s.
///
/// `ServiceRemoved` only carries the fullname, so the translator remembers which id each
/// resolved fullname announced (robust even if `mdns-sd` renamed the instance after a name
/// conflict) and falls back to parsing the id out of the instance name.
#[derive(Debug, Default)]
pub(crate) struct EventTranslator {
    ids_by_fullname: HashMap<String, String>,
}

impl EventTranslator {
    pub(crate) fn translate(&mut self, event: ServiceEvent) -> Option<DiscoveryEvent> {
        match event {
            ServiceEvent::ServiceResolved(service) if service.ty_domain == SERVICE_TYPE => {
                // Invalid/foreign TXT records are ignored: they are not wshare hosts we can use.
                let host = host_from_resolved(&service).ok()?;
                self.ids_by_fullname
                    .insert(service.fullname.clone(), host.announcement.id.clone());
                Some(DiscoveryEvent::Found(host))
            }
            ServiceEvent::ServiceRemoved(ty, fullname) if ty == SERVICE_TYPE => {
                let id = match self.ids_by_fullname.remove(&fullname) {
                    Some(id) => id,
                    None => id_from_fullname(&fullname)?.to_owned(),
                };
                // Another live instance still announces this id (e.g. a restarted host that
                // got a new instance name before the old one expired): keep the host.
                let still_present = self.ids_by_fullname.values().any(|other| *other == id);
                (!still_present).then_some(DiscoveryEvent::Lost { id })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance_name;
    use crate::txt::sample;
    use mdns_sd::ServiceInfo;
    use std::net::IpAddr;

    fn resolved(a: &HostAnnouncement, instance: &str, ip: &str) -> ServiceEvent {
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            instance,
            "pc-x.local.",
            ip,
            a.port,
            a.to_txt(),
        )
        .unwrap();
        ServiceEvent::ServiceResolved(Box::new(info.as_resolved_service()))
    }

    fn fullname(instance: &str) -> String {
        format!("{instance}.{SERVICE_TYPE}")
    }

    fn removed(instance: &str) -> ServiceEvent {
        ServiceEvent::ServiceRemoved(SERVICE_TYPE.into(), fullname(instance))
    }

    #[test]
    fn resolved_becomes_found() {
        let a = sample("abc123", "Desk");
        let mut t = EventTranslator::default();
        let event = t.translate(resolved(
            &a,
            &instance_name("Desk", "abc123"),
            "192.168.1.5",
        ));
        let expected = DiscoveredHost::new(a, vec!["192.168.1.5".parse::<IpAddr>().unwrap()]);
        assert_eq!(event, Some(DiscoveryEvent::Found(expected)));
    }

    #[test]
    fn other_protocol_version_is_still_found() {
        let mut a = sample("abc", "Old");
        a.protocol_version = 99;
        let mut t = EventTranslator::default();
        assert!(matches!(
            t.translate(resolved(&a, "Old-abc", "10.0.0.1")),
            Some(DiscoveryEvent::Found(h)) if h.announcement.protocol_version == 99
        ));
    }

    #[test]
    fn invalid_txt_is_ignored() {
        let info =
            ServiceInfo::new(SERVICE_TYPE, "x-abc", "h.local.", "10.0.0.1", 1, None).unwrap();
        let mut t = EventTranslator::default();
        let event = ServiceEvent::ServiceResolved(Box::new(info.as_resolved_service()));
        assert_eq!(t.translate(event), None);
    }

    #[test]
    fn removed_maps_back_to_id_via_resolved_name() {
        // Instance renamed by conflict resolution: id cannot be parsed, must come from the map.
        let a = sample("abc", "Desk");
        let mut t = EventTranslator::default();
        t.translate(resolved(&a, "Desk-abc (2)", "10.0.0.1"))
            .unwrap();
        assert_eq!(
            t.translate(removed("Desk-abc (2)")),
            Some(DiscoveryEvent::Lost { id: "abc".into() })
        );
        // Second removal of the same name is unknown and unparsable.
        assert_eq!(t.translate(removed("Desk-abc (2)")), None);
    }

    #[test]
    fn removed_without_resolve_parses_instance_name() {
        let mut t = EventTranslator::default();
        assert_eq!(
            t.translate(removed("My-PC-a1b2")),
            Some(DiscoveryEvent::Lost { id: "a1b2".into() })
        );
        assert_eq!(t.translate(removed("garbage")), None);
    }

    #[test]
    fn removed_keeps_host_announced_under_another_name() {
        let a = sample("abc", "Desk");
        let mut t = EventTranslator::default();
        t.translate(resolved(&a, "Desk-abc", "10.0.0.1")).unwrap();
        t.translate(resolved(&a, "Desk renamed-abc", "10.0.0.1"))
            .unwrap();
        assert_eq!(t.translate(removed("Desk-abc")), None);
        assert_eq!(
            t.translate(removed("Desk renamed-abc")),
            Some(DiscoveryEvent::Lost { id: "abc".into() })
        );
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut t = EventTranslator::default();
        let a = sample("abc", "x");
        let foreign = ServiceInfo::new(
            "_http._tcp.local.",
            "x-abc",
            "h.local.",
            "10.0.0.1",
            1,
            a.to_txt(),
        )
        .unwrap();
        for event in [
            ServiceEvent::SearchStarted(SERVICE_TYPE.into()),
            ServiceEvent::ServiceFound(SERVICE_TYPE.into(), fullname("x-abc")),
            ServiceEvent::SearchStopped(SERVICE_TYPE.into()),
            ServiceEvent::ServiceResolved(Box::new(foreign.as_resolved_service())),
            ServiceEvent::ServiceRemoved(
                "_http._tcp.local.".into(),
                "x-abc._http._tcp.local.".into(),
            ),
        ] {
            assert_eq!(t.translate(event), None);
        }
    }
}
