//! Carrying a connection to a name whose rule gives a credential: barre
//! terminates the VM's TLS with a certificate from its authority, reads each
//! HTTP/1.1 request, sets the credential's header, and sends it over its own
//! TLS connection to the real host. The VM never holds the credential.

use std::convert::Infallible;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{HOST, HeaderName, HeaderValue};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::rules;

type Body = BoxBody<Bytes, hyper::Error>;

/// TLS to real hosts: the public authorities, HTTP/1.1.
static CLIENT: LazyLock<Arc<ClientConfig>> = LazyLock::new(|| {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
});

/// What a carried connection needs from the server around it.
pub struct Carry<'a> {
    /// The name the VM connected to; requests must be for it.
    pub name: &'a str,
    /// Presents a certificate for `name`.
    pub tls: Arc<ServerConfig>,
    /// The credential's header, read again for each request.
    pub credential: &'a (dyn Fn() -> Result<(HeaderName, HeaderValue), String> + Sync),
    /// One log line per request.
    pub log: &'a (dyn Fn(Value) + Sync),
    pub handshake: Duration,
    pub idle: Duration,
}

/// Carries the VM's connection (`client`, with the bytes already read from
/// it in `head`) to `upstream`, request by request. Returns why it ended.
pub async fn carry(c: Carry<'_>, client: TcpStream, head: Vec<u8>, upstream: TcpStream) -> String {
    let client = Prefixed {
        head,
        pos: 0,
        inner: client,
    };
    let client = match tokio::time::timeout(c.handshake, TlsAcceptor::from(c.tls.clone()).accept(client)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return format!("VM's TLS handshake: {e}"),
        Err(_) => return "VM's TLS handshake timed out".into(),
    };
    let server_name = match ServerName::try_from(c.name.to_string()) {
        Ok(n) => n,
        Err(e) => return format!("bad name: {e}"),
    };
    let upstream = match tokio::time::timeout(
        c.handshake,
        TlsConnector::from(CLIENT.clone()).connect(server_name, upstream),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return format!("upstream TLS handshake: {e}"),
        Err(_) => return "upstream TLS handshake timed out".into(),
    };
    let (sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(upstream)).await {
        Ok(s) => s,
        Err(e) => return format!("upstream HTTP: {e}"),
    };
    let upstream_done = tokio::spawn(conn);
    let sender = Mutex::new(sender);

    let service = hyper::service::service_fn(|req: Request<Incoming>| {
        let c = &c;
        let sender = &sender;
        async move { Ok::<_, Infallible>(forward(c, sender, req).await) }
    });
    let served = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(c.idle)
        .keep_alive(true)
        .serve_connection(TokioIo::new(client), service)
        .await;
    upstream_done.abort();
    match served {
        Ok(()) => "done".into(),
        Err(e) if e.is_timeout() => "idle".into(),
        Err(e) => format!("VM's HTTP: {e}"),
    }
}

/// One request: checked, given the credential, sent, and its response
/// streamed back. Logged with its method, path and status, never the
/// credential.
async fn forward(
    c: &Carry<'_>,
    sender: &Mutex<hyper::client::conn::http1::SendRequest<Incoming>>,
    mut req: Request<Incoming>,
) -> Response<Body> {
    let started = Instant::now();
    let mut line = json!({
        "event": "request", "name": c.name,
        "method": req.method().as_str(), "path": req.uri().path(),
    });
    let refuse = |line: &mut Value, status: StatusCode, reason: String| {
        line["decision"] = json!("refuse");
        line["reason"] = json!(reason);
        line["status"] = json!(status.as_u16());
        (c.log)(line.clone());
        plain(status, &reason)
    };

    if let Err(reason) = check_host(&req, c.name) {
        return refuse(&mut line, StatusCode::MISDIRECTED_REQUEST, reason);
    }
    let (header, value) = match (c.credential)() {
        Ok(h) => h,
        Err(reason) => return refuse(&mut line, StatusCode::BAD_GATEWAY, reason),
    };
    set_credential(req.headers_mut(), header, value);

    let mut sender = sender.lock().await;
    if let Err(e) = sender.ready().await {
        return refuse(&mut line, StatusCode::BAD_GATEWAY, format!("upstream closed: {e}"));
    }
    match sender.send_request(req).await {
        Ok(resp) => {
            line["decision"] = json!("allow");
            line["status"] = json!(resp.status().as_u16());
            line["ms"] = json!(started.elapsed().as_millis() as u64);
            (c.log)(line);
            resp.map(BodyExt::boxed)
        }
        Err(e) => refuse(&mut line, StatusCode::BAD_GATEWAY, format!("upstream: {e}")),
    }
}

/// The request must be for the name the VM connected to.
fn check_host<B>(req: &Request<B>, name: &str) -> Result<(), String> {
    let host = req
        .headers()
        .get(HOST)
        .ok_or("no Host header")?
        .to_str()
        .map_err(|_| "unreadable Host header")?;
    let host = host.strip_suffix(":443").unwrap_or(host);
    match rules::normalize(host) {
        Some(h) if h == name => Ok(()),
        _ => Err(format!("Host {host:?} isn't {name}")),
    }
}

/// Replaces every header named `header` with the credential's value, so
/// nothing the VM sends in it reaches the host.
fn set_credential(headers: &mut hyper::HeaderMap, header: HeaderName, value: HeaderValue) {
    headers.remove(&header);
    headers.insert(header, value);
}

fn plain(status: StatusCode, reason: &str) -> Response<Body> {
    let body = Full::new(Bytes::from(format!("barre: {reason}\n")))
        .map_err(|never| match never {})
        .boxed();
    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    resp
}

/// The VM's stream, with the bytes barre read before deciding (the TLS
/// ClientHello) put back in front.
struct Prefixed {
    head: Vec<u8>,
    pos: usize,
    inner: TcpStream,
}

impl AsyncRead for Prefixed {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.pos < self.head.len() {
            let n = buf.remaining().min(self.head.len() - self.pos);
            let pos = self.pos;
            buf.put_slice(&self.head[pos..pos + n]);
            self.pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Prefixed {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(host: Option<&str>) -> Request<()> {
        let mut b = Request::builder().uri("/v1/messages");
        if let Some(h) = host {
            b = b.header(HOST, h);
        }
        b.body(()).unwrap()
    }

    #[test]
    fn requests_must_be_for_the_name() {
        assert!(check_host(&req(Some("api.example.com")), "api.example.com").is_ok());
        assert!(check_host(&req(Some("API.Example.com:443")), "api.example.com").is_ok());
        assert!(check_host(&req(Some("other.example.com")), "api.example.com").is_err());
        assert!(check_host(&req(Some("api.example.com:8443")), "api.example.com").is_err());
        assert!(check_host(&req(None), "api.example.com").is_err());
    }

    #[test]
    fn the_credential_replaces_what_the_vm_sent() {
        let mut h = hyper::HeaderMap::new();
        h.append("authorization", HeaderValue::from_static("Bearer fake"));
        h.append("authorization", HeaderValue::from_static("Basic also-fake"));
        h.insert("x-other", HeaderValue::from_static("kept"));
        set_credential(
            &mut h,
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer real"),
        );
        let all: Vec<_> = h.get_all("authorization").iter().collect();
        assert_eq!(all, ["Bearer real"]);
        assert_eq!(h["x-other"], "kept");
    }

    /// A client trusting only the authority completes a handshake with the
    /// certificate it makes for a name, and agrees on HTTP/1.1.
    #[tokio::test]
    async fn the_authoritys_certificates_verify() {
        let dir = std::env::temp_dir().join(format!("barre-intercept-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::ca::init(&dir).unwrap();
        let ca = crate::ca::Ca::load(&dir).unwrap();
        let server = ca.server_config("api.example.com").unwrap();

        use rustls::pki_types::CertificateDer;
        use rustls::pki_types::pem::PemObject;
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_file(dir.join("ca.crt")).unwrap())
            .unwrap();
        let mut client = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        client.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

        let (a, b) = tokio::io::duplex(64 * 1024);
        let accept = tokio::spawn(async move { TlsAcceptor::from(server).accept(a).await.map(drop) });
        let tls = TlsConnector::from(Arc::new(client.clone()))
            .connect(ServerName::try_from("api.example.com").unwrap(), b)
            .await
            .unwrap();
        assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
        accept.await.unwrap().unwrap();

        // Another name doesn't verify against this certificate.
        let server = ca.server_config("api.example.com").unwrap();
        let (a, b) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move { TlsAcceptor::from(server).accept(a).await.map(drop) });
        assert!(
            TlsConnector::from(Arc::new(client))
                .connect(ServerName::try_from("evil.example.com").unwrap(), b)
                .await
                .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
