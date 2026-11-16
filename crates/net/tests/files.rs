//! File streams over 127.0.0.1: header and data arrive whole, either side
//! can give a file up, and the number of streams open at once is bounded.

use std::net::Ipv4Addr;
use std::time::Duration;

use net::{FileChannel, HostEndpoint, Identity, connect};
use protocol::control::{ControlMessage, PROTOCOL_VERSION};
use protocol::transfer::{FileEntry, MAX_FILE_STREAMS, StreamHeader, TransferId};
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(10);

/// The file channels of a connected host (first) and viewer (second). The
/// endpoint and the control senders must outlive the test.
struct Pair {
    _host: HostEndpoint,
    _controls: (net::ControlSender, net::ControlSender),
    host: FileChannel,
    viewer: FileChannel,
}

async fn pair() -> Pair {
    let identity = Identity::generate("host").unwrap();
    let host = HostEndpoint::bind((Ipv4Addr::LOCALHOST, 0).into(), &identity).unwrap();
    let addr = host.local_addr().unwrap();
    timeout(TIMEOUT, async {
        let (accepted, viewer) = tokio::join!(host.accept(), async {
            let session = connect(addr, Some(identity.fingerprint()), None)
                .await
                .unwrap();
            let (mut tx, _rx, _video, files) = session.split();
            tx.send(&ControlMessage::Hello {
                version: PROTOCOL_VERSION,
                viewer_name: "v".into(),
                pin: None,
            })
            .await
            .unwrap();
            (tx, files)
        });
        let (host_tx, mut host_rx, _video, host_files) = accepted.unwrap().split();
        host_rx.recv().await.unwrap();
        let (viewer_tx, viewer_files) = viewer;
        Pair {
            _host: host,
            _controls: (host_tx, viewer_tx),
            host: host_files,
            viewer: viewer_files,
        }
    })
    .await
    .expect("connection timed out")
}

fn header(id: u32, size: u64) -> StreamHeader {
    StreamHeader {
        id: TransferId(id),
        index: 0,
        entry: FileEntry::file("cartella/file è.bin", size),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_arrives_with_its_header_and_all_its_bytes() {
    let pair = pair().await;
    let data: Vec<u8> = (0..3_000_000u32).map(|i| i as u8).collect();
    let sent = data.clone();
    let sender = tokio::spawn({
        let files = pair.viewer.clone();
        async move {
            let mut stream = files.open(&header(1, sent.len() as u64)).await.unwrap();
            for chunk in sent.chunks(100_000) {
                stream.write(chunk).await.unwrap();
            }
            stream.finish().await.unwrap();
        }
    });
    timeout(TIMEOUT, async {
        let (received, mut stream) = pair.host.accept().await.unwrap().open().await.unwrap();
        assert_eq!(received, header(1, data.len() as u64));
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.read().await.unwrap() {
            assert!(chunk.len() <= net::files::MAX_READ_CHUNK);
            bytes.extend_from_slice(&chunk);
        }
        assert_eq!(bytes, data);
        sender.await.unwrap();
    })
    .await
    .expect("transfer timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_the_sender_gives_up_fails_on_the_receiver() {
    let pair = pair().await;
    timeout(TIMEOUT, async {
        let mut stream = pair.host.open(&header(2, 1000)).await.unwrap();
        stream.write(&[1; 10]).await.unwrap();
        let (_, mut incoming) = pair.viewer.accept().await.unwrap().open().await.unwrap();
        stream.cancel();
        let mut outcome = incoming.read().await;
        while let Ok(Some(_)) = outcome {
            outcome = incoming.read().await;
        }
        assert!(outcome.is_err(), "a reset stream is not a finished file");
    })
    .await
    .expect("timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_the_receiver_gives_up_fails_on_the_sender() {
    let pair = pair().await;
    timeout(TIMEOUT, async {
        let mut stream = pair.host.open(&header(3, 1 << 30)).await.unwrap();
        stream.write(&[1; 10]).await.unwrap();
        let (_, incoming) = pair.viewer.accept().await.unwrap().open().await.unwrap();
        incoming.cancel();
        let chunk = vec![0; 1 << 20];
        let mut failed = false;
        for _ in 0..1024 {
            if stream.write(&chunk).await.is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed, "writes stop once the receiver gave the file up");
    })
    .await
    .expect("timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_more_streams_than_allowed_are_open_at_once() {
    let pair = pair().await;
    timeout(TIMEOUT, async {
        let mut open = Vec::new();
        for id in 0..MAX_FILE_STREAMS {
            open.push(pair.host.open(&header(id, 1)).await.unwrap());
        }
        let extra =
            tokio::time::timeout(Duration::from_millis(300), pair.host.open(&header(99, 1))).await;
        assert!(extra.is_err(), "the next stream waits for a free one");

        // Finishing one (and the peer reading it to the end) frees a place.
        let first = open.remove(0);
        let reader = tokio::spawn({
            let files = pair.viewer.clone();
            async move {
                let (_, mut stream) = files.accept().await.unwrap().open().await.unwrap();
                while stream.read().await.unwrap().is_some() {}
            }
        });
        first.finish().await.unwrap();
        reader.await.unwrap();
        pair.host.open(&header(99, 1)).await.unwrap();
    })
    .await
    .expect("timed out");
}
