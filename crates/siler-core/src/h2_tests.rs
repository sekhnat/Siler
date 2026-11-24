//! HTTP/2 slots against a local https server (`testdata/`: a test CA and a certificate for
//! 127.0.0.1 it signed, made with openssl, valid for a century, used nowhere else).

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use hyper::{Request, Response, StatusCode, Version};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::{Engine, NewDownload, Status};

const CA: &[u8] = include_bytes!("testdata/ca.pem");
const CERT: &[u8] = include_bytes!("testdata/server.pem");
const KEY: &[u8] = include_bytes!("testdata/server.key");
const SIZE: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct Seen {
    /// TCP connections accepted.
    accepts: AtomicUsize,
    h1: AtomicUsize,
    h2: AtomicUsize,
}

fn data() -> Arc<Vec<u8>> {
    Arc::new((0..SIZE).map(|i| (i * 31 % 251) as u8).collect())
}

/// Serves `data` with ranges, over HTTP/2 or HTTP/1.1 as the client offers, paced so a
/// download takes a moment.
async fn serve(data: Arc<Vec<u8>>) -> (SocketAddr, Arc<Seen>) {
    let certs = vec![CertificateDer::from_pem_slice(CERT).unwrap()];
    let key = PrivateKeyDer::from_pem_slice(KEY).unwrap();
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Seen::default());
    let s = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            s.accepts.fetch_add(1, SeqCst);
            let (acceptor, data, s) = (acceptor.clone(), data.clone(), s.clone());
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let service = hyper::service::service_fn(move |req| {
                    let (data, s) = (data.clone(), s.clone());
                    async move { Ok::<_, Infallible>(answer(req, &data, &s)) }
                });
                let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(tls), service)
                    .await;
            });
        }
    });
    (addr, seen)
}

type Body = http_body_util::combinators::BoxBody<Bytes, Infallible>;

fn answer(req: Request<hyper::body::Incoming>, data: &Arc<Vec<u8>>, seen: &Seen) -> Response<Body> {
    match req.version() {
        Version::HTTP_2 => seen.h2.fetch_add(1, SeqCst),
        _ => seen.h1.fetch_add(1, SeqCst),
    };
    let range = req
        .headers()
        .get(hyper::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .map(|(a, b)| {
            let a: usize = a.parse().unwrap();
            let b: usize = b.parse().map_or(SIZE - 1, |b: usize| b.min(SIZE - 1));
            (a, b)
        });
    let (status, start, end) = match range {
        Some((a, b)) => (StatusCode::PARTIAL_CONTENT, a, b),
        None => (StatusCode::OK, 0, SIZE - 1),
    };
    let data = data.clone();
    // 64 KiB every 10 ms: about 6 MB/s per stream.
    let chunks = futures_util::stream::iter((start..=end).step_by(64 * 1024)).then(move |at| {
        let data = data.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let stop = (at + 64 * 1024).min(end + 1);
            Ok::<_, Infallible>(Frame::data(Bytes::copy_from_slice(&data[at..stop])))
        }
    });
    let mut resp = Response::builder()
        .status(status)
        .header(hyper::header::ACCEPT_RANGES, "bytes")
        .header(hyper::header::CONTENT_LENGTH, end + 1 - start);
    if range.is_some() {
        resp = resp.header(
            hyper::header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{SIZE}"),
        );
    }
    resp.body(BodyExt::boxed(StreamBody::new(chunks))).unwrap()
}

async fn engine(dir: &std::path::Path, slots: u8) -> Engine {
    let e = Engine::open(dir.join("data")).unwrap();
    e.inner
        .net
        .test_roots
        .lock()
        .unwrap()
        .push(reqwest::Certificate::from_pem(CA).unwrap());
    let mut s = e.settings();
    s.download_dir = dir.join("dl");
    s.use_category_folders = false;
    s.min_split_size = 64 * 1024;
    s.connections = 8;
    s.http2_connections_per_server = slots;
    e.set_settings(s).unwrap();
    e
}

/// Downloads the file, and returns what the server saw and the protocol the download showed.
async fn download(slots: u8) -> (Arc<Seen>, Option<u8>) {
    let data = data();
    let (addr, seen) = serve(data.clone()).await;
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), slots).await;
    let id = e
        .add(NewDownload {
            url: format!("https://{addr}/file.bin"),
            ..Default::default()
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut shown = None;
    let d = loop {
        let d = e.get(id).unwrap();
        shown = shown.or(d.protocol);
        if matches!(d.status, Status::Completed | Status::Failed) {
            break d;
        }
        assert!(Instant::now() < deadline, "timed out: {d:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let got = std::fs::read(tmp.path().join("dl/file.bin")).unwrap();
    assert!(got == *data, "the file differs");
    (seen, shown)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ranges_share_the_http2_slots() {
    let (seen, shown) = download(2).await;
    let (accepts, h1, h2) = (
        seen.accepts.load(SeqCst),
        seen.h1.load(SeqCst),
        seen.h2.load(SeqCst),
    );
    // The probe stays on HTTP/1.1, on a connection of its own; eight streams share two.
    assert_eq!(h1, 1, "only the probe is HTTP/1.1");
    assert!(h2 >= 8, "{h2} HTTP/2 requests");
    assert!(accepts <= 3, "{accepts} connections");
    assert_eq!(shown, Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn off_is_http1_with_a_connection_per_stream() {
    let (seen, shown) = download(0).await;
    assert_eq!(seen.h2.load(SeqCst), 0);
    assert!(
        seen.accepts.load(SeqCst) >= 8,
        "{}",
        seen.accepts.load(SeqCst)
    );
    assert_eq!(shown, Some(1));
}
