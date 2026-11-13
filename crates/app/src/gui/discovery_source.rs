//! Feeds LAN discovery events to the interface from a background thread.

use std::time::{Duration, Instant};

use discovery::Browser;

use super::ports::{AppEvent, Inbox};

const WAIT: Duration = Duration::from_secs(1);
/// Delay before restarting a browser that failed or stopped.
const RETRY: Duration = Duration::from_secs(10);

/// Starts browsing in a detached thread that lives as long as the process.
pub fn spawn(inbox: Inbox) {
    let _ = std::thread::Builder::new().name("discovery".into()).spawn(move || {
        loop {
            match Browser::start() {
                Ok(browser) => browse(browser, &inbox),
                Err(e) => inbox.send(AppEvent::DiscoveryFailed(format!("{e:#}"))),
            }
            std::thread::sleep(RETRY);
        }
    });
}

/// Forwards events until the browser stops (an empty wait that returns
/// early means its daemon is gone).
fn browse(mut browser: Browser, inbox: &Inbox) {
    loop {
        let started = Instant::now();
        match browser.recv_timeout(WAIT) {
            Some(event) => inbox.send(AppEvent::Discovery(event)),
            None if started.elapsed() < WAIT / 2 => return,
            None => {}
        }
    }
}
