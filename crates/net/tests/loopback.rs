//! End-to-end tests over 127.0.0.1.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use net::{Fingerprint, HostEndpoint, Identity, connect};
use pipeline::DatagramSink;
use protocol::control::{ControlMessage, PROTOCOL_VERSION, StreamInfo, VideoCodec};
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(10);

fn hello() -> ControlMessage {
    ControlMessage::Hello {
        version: PROTOCOL_VERSION,
        viewer_name: "viewer".into(),
        pin: "1234".into(),
    }
}

fn welcome() -> ControlMessage {
    ControlMessage::Welcome {
        host_name: "host".into(),
        stream: StreamInfo {
            codec: VideoCodec::H264,
            width: 1920,
            height: 1080,
            fps: 60,
        },
        input_allowed: true,
    }
}

fn bind_host(identity: &Identity) -> (HostEndpoint, SocketAddr) {
    let host = HostEndpoint::bind((Ipv4Addr::LOCALHOST, 0).into(), identity).unwrap();
    let addr = host.local_addr().unwrap();
    (host, addr)
}

type Parts = (net::ControlSender, net::ControlReceiver, net::VideoChannel);

/// Host endpoint + both sides already split; Hello has been delivered and
/// read by the host.
struct Connected {
    _host: HostEndpoint,
    identity: Identity,
    host: Parts,
    host_handle: net::SessionHandle,
    viewer: Parts,
    viewer_fingerprint: Option<Fingerprint>,
    viewer_remote: SocketAddr,
}

async fn connected() -> Connected {
    let identity = Identity::generate("host").unwrap();
    let (host, addr) = bind_host(&identity);
    timeout(TIMEOUT, async {
        let (host_session, viewer_session) = tokio::join!(host.accept(), async {
            let viewer = connect(addr, Some(identity.fingerprint())).await.unwrap();
            let fp = viewer.peer_fingerprint();
            let remote = viewer.remote_addr();
            let (mut tx, rx, video) = viewer.split();
            tx.send(&hello()).await.unwrap();
            ((tx, rx, video), fp, remote)
        });
        let host_session = host_session.unwrap();
        assert!(host_session.peer_fingerprint().is_none());
        let host_handle = host_session.handle();
        let (host_tx, mut host_rx, host_video) = host_session.split();
        assert_eq!(host_rx.recv().await.unwrap(), Some(hello()));
        let (viewer, viewer_fingerprint, viewer_remote) = viewer_session;
        Connected {
            _host: host,
            identity,
            host: (host_tx, host_rx, host_video),
            host_handle,
            viewer,
            viewer_fingerprint,
            viewer_remote,
        }
    })
    .await
    .expect("connection timed out")
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_and_accept() {
    let c = connected().await;
    assert_eq!(c.viewer_fingerprint, Some(c.identity.fingerprint()));
    assert_eq!(c.viewer_remote, c._host.local_addr().unwrap());
    assert!(c.host_handle.remote_addr().ip().is_loopback());
}

#[tokio::test(flavor = "multi_thread")]
async fn control_roundtrip_both_directions() {
    let mut c = connected().await;
    timeout(TIMEOUT, async {
        c.host.0.send(&welcome()).await.unwrap();
        assert_eq!(c.viewer.1.recv().await.unwrap(), Some(welcome()));

        c.viewer
            .0
            .send(&ControlMessage::RequestKeyframe)
            .await
            .unwrap();
        c.viewer.0.send(&ControlMessage::Bye).await.unwrap();
        assert_eq!(
            c.host.1.recv().await.unwrap(),
            Some(ControlMessage::RequestKeyframe)
        );
        assert_eq!(c.host.1.recv().await.unwrap(), Some(ControlMessage::Bye));

        // Finishing the stream is a clean end for the peer.
        c.viewer.0.finish().unwrap();
        assert_eq!(c.host.1.recv().await.unwrap(), None);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn application_close_is_clean_end() {
    let mut c = connected().await;
    timeout(TIMEOUT, async {
        c.host_handle.close("done");
        assert_eq!(c.viewer.1.recv().await.unwrap(), None);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn datagram_roundtrip_including_max_size() {
    let c = connected().await;
    let host_video = &c.host.2;
    let viewer_video = &c.viewer.2;
    timeout(TIMEOUT, async {
        DatagramSink::send(host_video, b"small".to_vec()).unwrap();
        assert_eq!(&viewer_video.recv().await.unwrap()[..], b"small");

        let max = DatagramSink::max_datagram_size(host_video);
        assert!(max >= 1000, "max datagram size {max} unexpectedly small");
        let big: Vec<u8> = (0..max).map(|i| i as u8).collect();
        DatagramSink::send(host_video, big.clone()).unwrap();
        assert_eq!(viewer_video.recv().await.unwrap(), big);

        // Viewer -> host works too (e.g. for future feedback packets).
        DatagramSink::send(viewer_video, vec![42]).unwrap();
        assert_eq!(&host_video.recv().await.unwrap()[..], [42]);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_datagram_errors() {
    let c = connected().await;
    let max = DatagramSink::max_datagram_size(&c.host.2);
    assert!(DatagramSink::send(&c.host.2, vec![0; max + 1]).is_err());
    assert!(DatagramSink::send(&c.host.2, vec![0; 65_536]).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn pinned_fingerprint_mismatch_fails() {
    let identity = Identity::generate("host").unwrap();
    let (host, addr) = bind_host(&identity);
    let host_task = tokio::spawn(async move {
        let _ = host.accept().await;
    });
    let wrong = Identity::generate("impostor").unwrap().fingerprint();
    let result = timeout(TIMEOUT, connect(addr, Some(wrong))).await.unwrap();
    assert!(result.is_err());
    host_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn tofu_reports_host_fingerprint() {
    let identity = Identity::generate("host").unwrap();
    let (host, addr) = bind_host(&identity);
    let host_task = tokio::spawn(async move {
        let _ = host.accept().await;
    });
    let session = timeout(TIMEOUT, connect(addr, None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.peer_fingerprint(), Some(identity.fingerprint()));
    session.close();
    host_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn host_keeps_accepting_after_failed_handshake() {
    let identity = Identity::generate("host").unwrap();
    let (host, addr) = bind_host(&identity);
    let wrong = Identity::generate("impostor").unwrap().fingerprint();
    timeout(TIMEOUT, async {
        // The viewer parts are returned (not dropped) so the connection
        // stays open while the host reads Hello.
        let (accepted, _viewer) = tokio::join!(host.accept(), async {
            assert!(connect(addr, Some(wrong)).await.is_err());
            let viewer = connect(addr, Some(identity.fingerprint())).await.unwrap();
            let (mut tx, rx, video) = viewer.split();
            tx.send(&hello()).await.unwrap();
            (tx, rx, video)
        });
        let (_tx, mut rx, _video) = accepted.unwrap().split();
        assert_eq!(rx.recv().await.unwrap(), Some(hello()));
    })
    .await
    .unwrap();
}
