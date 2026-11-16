//! Controller behaviour around the computers this PC knows: recent and
//! offline ones, their stored address and MAC, favorites and Wake-on-LAN.

use discovery::{DiscoveredHost, DiscoveryEvent, HostAnnouncement, MacAddress};
use protocol::control::PROTOCOL_VERSION;

use super::fakes::{Shared, TestController, calls, controller_knowing, short};
use super::*;
use crate::gui::remembered::tests::known;
use crate::gui::settings::ViewerDisplay;
use crate::trust::KnownHost;

const LAST_ADDRESS: &str = "10.0.0.5:7420";
const MAC_TEXT: &str = "aa:bb:cc:00:11:22";

fn mac() -> MacAddress {
    MAC_TEXT.parse().unwrap()
}

/// A known host with an address and MAC, not announced on the LAN.
fn offline_office() -> KnownHost {
    let mut office = known(0xab, "UFFICIO");
    office.address = Some(LAST_ADDRESS.parse().unwrap());
    office.mac = Some(mac());
    office
}

fn with_offline_office() -> (TestController, Shared, KnownHost, String) {
    let office = offline_office();
    let id = office.fingerprint.host_id();
    let (c, log) = controller_knowing(vec![office.clone()]);
    (c, log, office, id)
}

fn announce(id: &str, ip: &str, mac: Option<MacAddress>) -> AppEvent {
    AppEvent::Discovery(DiscoveryEvent::Found(DiscoveredHost::new(
        HostAnnouncement {
            id: id.into(),
            display_name: "UFFICIO".into(),
            port: 7420,
            protocol_version: PROTOCOL_VERSION,
            width: 1920,
            height: 1080,
            mac,
        },
        vec![ip.parse().unwrap()],
    )))
}

#[test]
fn a_known_host_not_announced_is_listed_offline_and_connects_at_its_last_address() {
    let (mut c, log, _office, id) = with_offline_office();
    let row = c.state().host_row(&id).expect("listed");
    assert!(!row.online && row.known && row.connectable());

    c.handle(Intent::ConnectHost {
        id: id.clone(),
        pin: String::new(),
    });
    assert_eq!(
        log.borrow().launched.last().unwrap().1,
        LaunchRequest {
            target: LAST_ADDRESS.parse().unwrap(),
            host_id: Some(id),
            pin: None,
            display: ViewerDisplay::default(),
        }
    );
    assert_eq!(c.state().connect_error, None);
}

#[test]
fn an_offline_host_without_a_last_address_says_it_has_none() {
    let mut office = known(0xab, "UFFICIO");
    office.mac = Some(mac());
    let id = office.fingerprint.host_id();
    let (mut c, log) = controller_knowing(vec![office]);
    c.handle(Intent::ConnectHost {
        id,
        pin: String::new(),
    });
    assert!(log.borrow().launched.is_empty());
    assert_eq!(
        c.state().connect_error.as_deref(),
        Some(text::HOST_NO_ADDRESS)
    );
}

#[test]
fn the_announcement_of_a_known_host_stores_its_address_and_mac_once() {
    let (mut c, log, office, id) = with_offline_office();
    let key = short(&office.fingerprint);

    c.on_event(announce(&id, "10.0.0.9", Some(mac())));
    assert_eq!(calls(&log), [format!("note {key}")]);
    let stored = log.borrow().known[0].address;
    assert_eq!(stored, Some("10.0.0.9:7420".parse().unwrap()));

    c.on_event(announce(&id, "10.0.0.9", Some(mac())));
    assert!(
        calls(&log).is_empty(),
        "already up to date: nothing written"
    );
    assert_eq!(
        c.state().remembered.known(&id).unwrap().address,
        Some("10.0.0.9:7420".parse().unwrap()),
        "the interface sees the stored address"
    );
}

#[test]
fn an_announcement_without_mac_keeps_the_known_one() {
    let (mut c, log, office, id) = with_offline_office();
    c.on_event(announce(&id, "10.0.0.9", None));
    assert_eq!(
        calls(&log),
        [format!("note {}", short(&office.fingerprint))]
    );
    assert_eq!(log.borrow().known[0].mac, Some(mac()));
}

#[test]
fn unknown_hosts_are_not_stored_by_their_announcement() {
    let (mut c, log) = controller_knowing(vec![]);
    c.on_event(announce(
        &known(1, "X").fingerprint.host_id(),
        "10.0.0.9",
        Some(mac()),
    ));
    assert!(calls(&log).is_empty());
}

#[test]
fn a_failed_update_of_the_address_is_reported_not_swallowed() {
    let (mut c, log, _office, id) = with_offline_office();
    log.borrow_mut().fail_note = true;
    c.on_event(announce(&id, "10.0.0.9", Some(mac())));
    let warning = c.state().remembered.warning().expect("reported");
    assert!(warning.contains("access denied"), "{warning}");
}

#[test]
fn a_first_connection_stores_the_mac_the_host_announced() {
    let mut office = known(0xcd, "UFFICIO");
    office.address = Some("192.168.1.3:7420".parse().unwrap());
    let id = office.fingerprint.host_id();
    let (mut c, log) = controller_knowing(vec![]);
    c.on_event(announce(&id, "192.168.1.3", Some(mac())));
    c.handle(Intent::ConnectHost {
        id: id.clone(),
        pin: "123456".into(),
    });
    assert!(calls(&log).is_empty(), "not known yet: nothing to store");

    // The viewer process writes the entry, then reports it connected.
    log.borrow_mut().known.push(office);
    c.on_event(AppEvent::Viewer {
        id: 1,
        update: ViewerUpdate::Connected,
    });
    assert_eq!(log.borrow().known[0].mac, Some(mac()));
    assert_eq!(c.state().host_row(&id).unwrap().mac, Some(mac()));
}

#[test]
fn waking_sends_the_signal_with_the_last_address() {
    let (mut c, log, _office, id) = with_offline_office();
    c.handle(Intent::WakeHost { id });
    assert_eq!(
        log.borrow().woken,
        [(
            mac(),
            Some(LAST_ADDRESS.parse::<std::net::SocketAddr>().unwrap().ip())
        )]
    );
    let notice = c.state().wake_notice.as_deref().expect("said so");
    assert!(notice.contains("UFFICIO") && notice.contains("Wake-on-LAN"));
    assert_eq!(c.state().connect_error, None);
}

#[test]
fn the_notice_goes_away_when_another_row_is_selected() {
    let (mut c, _log, _office, id) = with_offline_office();
    c.handle(Intent::WakeHost { id });
    c.handle(Intent::SelectHost(None));
    assert_eq!(c.state().wake_notice, None);
}

#[test]
fn waking_without_a_known_mac_explains_what_is_missing() {
    let mut office = known(0xab, "UFFICIO");
    office.address = Some(LAST_ADDRESS.parse().unwrap());
    let id = office.fingerprint.host_id();
    let (mut c, log) = controller_knowing(vec![office]);
    c.handle(Intent::WakeHost { id });
    assert!(log.borrow().woken.is_empty());
    assert_eq!(c.state().connect_error.as_deref(), Some(text::WAKE_NO_MAC));
}

#[test]
fn a_failed_signal_is_reported() {
    let (mut c, log, _office, id) = with_offline_office();
    log.borrow_mut().fail_wake = true;
    c.handle(Intent::WakeHost { id });
    let error = c.state().connect_error.as_deref().expect("reported");
    assert!(error.contains("UFFICIO") && error.contains("network is down"));
    assert_eq!(c.state().wake_notice, None);
}

#[test]
fn nothing_is_sent_to_a_computer_that_is_online_or_unknown() {
    let (mut c, log, _office, id) = with_offline_office();
    c.on_event(announce(
        &id,
        LAST_ADDRESS.split(':').next().unwrap(),
        Some(mac()),
    ));
    c.handle(Intent::WakeHost { id });
    c.handle(Intent::WakeHost {
        id: "ffffffffffffffff".into(),
    });
    assert!(log.borrow().woken.is_empty());
    assert_eq!(c.state().connect_error.as_deref(), Some(text::HOST_GONE));
}

#[test]
fn favorites_are_saved_and_the_list_follows() {
    let (mut c, log, office, id) = with_offline_office();
    c.handle(Intent::SetFavorite {
        id: id.clone(),
        favorite: true,
    });
    assert_eq!(
        calls(&log),
        [format!("favorite {} true", short(&office.fingerprint))]
    );
    assert!(c.state().host_row(&id).unwrap().favorite);
    c.handle(Intent::SetFavorite {
        id: id.clone(),
        favorite: false,
    });
    assert!(!c.state().host_row(&id).unwrap().favorite);
}

#[test]
fn a_failed_favorite_change_is_reported() {
    let (mut c, log, _office, id) = with_offline_office();
    log.borrow_mut().fail_favorite = true;
    c.handle(Intent::SetFavorite { id, favorite: true });
    assert!(
        c.state()
            .connect_error
            .as_deref()
            .unwrap()
            .contains("access denied")
    );
}

#[test]
fn removing_an_offline_host_takes_it_off_the_list() {
    let (mut c, log, office, id) = with_offline_office();
    c.handle(Intent::SelectHost(Some(id.clone())));
    c.handle(Intent::ForgetHost { id: id.clone() });
    assert_eq!(
        calls(&log),
        [format!("forget_host {}", short(&office.fingerprint))]
    );
    assert!(c.state().host_row(&id).is_none());
    assert_eq!(c.state().selected_host, None);
}
