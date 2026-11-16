//! Real mDNS round trip on this machine: advertise, browse, then withdraw.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use discovery::{Advertiser, Browser, DiscoveryEvent, HostAnnouncement};

fn wait_for(
    browser: &mut Browser,
    timeout: Duration,
    mut pred: impl FnMut(&DiscoveryEvent) -> bool,
) -> Option<DiscoveryEvent> {
    let deadline = Instant::now() + timeout;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match browser.recv_timeout(left) {
            Some(event) if pred(&event) => return Some(event),
            Some(_) => {}
            None => return None,
        }
    }
    None
}

#[test]
fn advertise_then_browse_then_lost() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = format!("t{:x}{:x}", std::process::id(), nanos & 0xffff_ffff);
    let announcement = HostAnnouncement {
        id: id.clone(),
        display_name: "wshare test".into(),
        port: 45_678,
        protocol_version: 1,
        width: 1920,
        height: 1080,
        mac: "aa:bb:cc:00:11:22".parse().ok(),
    };

    let mut browser = Browser::start().expect("browser");
    let advertiser = Advertiser::start(&announcement).expect("advertiser");

    let found = wait_for(
        &mut browser,
        Duration::from_secs(10),
        |e| matches!(e, DiscoveryEvent::Found(h) if h.announcement.id == id),
    )
    .expect("host not found within 10s");
    let DiscoveryEvent::Found(host) = found else {
        unreachable!()
    };
    assert_eq!(host.announcement, announcement);
    assert!(
        host.preferred_address().is_some(),
        "no usable address: {:?}",
        host.addresses
    );

    drop(advertiser);
    wait_for(
        &mut browser,
        Duration::from_secs(10),
        |e| matches!(e, DiscoveryEvent::Lost { id: lost } if *lost == id),
    )
    .expect("host not lost within 10s after dropping the advertiser");
}
