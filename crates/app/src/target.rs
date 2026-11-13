//! Turning the user's `view` argument into a host address.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use discovery::{Browser, DiscoveredHost, HostList};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Address(SocketAddr),
    /// Case-insensitive substring of a discovered host's name.
    Name(String),
    /// No preference: the only host on the LAN.
    Any,
}

/// Parses `IP`, `IP:PORT` or a host name. Pure.
pub fn parse_target(text: Option<&str>, default_port: u16) -> Target {
    let Some(text) = text else {
        return Target::Any;
    };
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return Target::Address(addr);
    }
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Target::Address(SocketAddr::new(ip, default_port));
    }
    Target::Name(text.to_string())
}

/// Picks the host matching `target` among discovered ones. Pure.
pub fn choose<'a>(hosts: &[&'a DiscoveredHost], target: &Target) -> Result<&'a DiscoveredHost> {
    let candidates: Vec<&DiscoveredHost> = match target {
        Target::Address(_) => unreachable!("addresses need no discovery"),
        Target::Any => hosts.to_vec(),
        Target::Name(name) => {
            let needle = name.to_lowercase();
            hosts
                .iter()
                .copied()
                .filter(|h| h.announcement.display_name.to_lowercase().contains(&needle))
                .collect()
        }
    };
    match candidates.as_slice() {
        [one] => Ok(one),
        [] => bail!("no matching host found on the LAN (try `wshare view <IP>`)"),
        many => {
            let names: Vec<&str> =
                many.iter().map(|h| h.announcement.display_name.as_str()).collect();
            bail!("several hosts found: {} - pass the name to choose", names.join(", "))
        }
    }
}

/// Browses the LAN for `wait` and returns what was found.
pub fn discover(wait: Duration) -> Result<HostList> {
    let mut browser = Browser::start()?;
    let mut hosts = HostList::default();
    let deadline = Instant::now() + wait;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if let Some(event) = browser.recv_timeout(left) {
            hosts.apply(event);
        }
    }
    Ok(hosts)
}

/// Resolves a target to a socket address, discovering if needed.
pub fn resolve(target: &Target, wait: Duration) -> Result<(SocketAddr, String)> {
    if let Target::Address(addr) = target {
        return Ok((*addr, addr.to_string()));
    }
    let list = discover(wait)?;
    let host = choose(&list.hosts(), target)?;
    let Some(addr) = host.preferred_address() else {
        bail!("{} announced no usable address", host.announcement.display_name);
    };
    Ok((addr, host.announcement.display_name.clone()))
}

#[cfg(test)]
mod tests {
    use discovery::HostAnnouncement;

    use super::*;

    fn host(id: &str, name: &str) -> DiscoveredHost {
        DiscoveredHost::new(
            HostAnnouncement {
                id: id.into(),
                display_name: name.into(),
                port: 7420,
                protocol_version: 1,
                width: 1920,
                height: 1080,
            },
            vec!["192.168.1.10".parse().unwrap()],
        )
    }

    #[test]
    fn parses_addresses_and_names() {
        assert_eq!(parse_target(None, 7420), Target::Any);
        assert_eq!(
            parse_target(Some("10.0.0.5"), 7420),
            Target::Address("10.0.0.5:7420".parse().unwrap())
        );
        assert_eq!(
            parse_target(Some("10.0.0.5:9000"), 7420),
            Target::Address("10.0.0.5:9000".parse().unwrap())
        );
        assert_eq!(parse_target(Some("laptop"), 7420), Target::Name("laptop".into()));
    }

    #[test]
    fn choose_by_partial_case_insensitive_name() {
        let (a, b) = (host("a1", "Office-PC"), host("b2", "Jacob-Laptop"));
        let hosts = vec![&a, &b];
        let chosen = choose(&hosts, &Target::Name("laptop".into())).unwrap();
        assert_eq!(chosen.id(), "b2");
    }

    #[test]
    fn any_requires_exactly_one_host() {
        let (a, b) = (host("a1", "A"), host("b2", "B"));
        assert_eq!(choose(&[&a], &Target::Any).unwrap().id(), "a1");
        assert!(choose(&[], &Target::Any).is_err());
        let err = choose(&[&a, &b], &Target::Any).unwrap_err().to_string();
        assert!(err.contains("A, B"), "{err}");
    }

    #[test]
    fn unknown_name_is_an_error() {
        let a = host("a1", "A");
        assert!(choose(&[&a], &Target::Name("zzz".into())).is_err());
    }
}
