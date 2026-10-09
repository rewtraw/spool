//! A single NNTP connection: connect, authenticate, fetch article bodies.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    /// Leave on. Off is only for a server with a certificate you have chosen to trust anyway.
    pub tls_verify: bool,
    pub username: String,
    pub password: String,
    pub connections: u32,
    /// 0 is tried first. Higher numbers are only asked for articles every lower number lacks.
    pub priority: u32,
    pub enabled: bool,
    /// BODY commands sent before reading replies. 1 disables pipelining.
    pub pipeline: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            id: String::new(),
            name: String::new(),
            host: String::new(),
            port: 563,
            tls: true,
            tls_verify: true,
            username: String::new(),
            password: String::new(),
            connections: 8,
            priority: 0,
            enabled: true,
            pipeline: 2,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum FetchError {
    /// The server does not have the article. Another server might.
    #[error("article not found")]
    Missing,
    /// The connection is unusable and must be reopened.
    #[error("connection error: {0}")]
    Io(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("unexpected response: {0}")]
    Protocol(String),
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

pub struct Connection {
    stream: Box<dyn Stream>,
    buf: Vec<u8>,
    pos: usize,
}

const IO_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_config(verify: bool) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("default TLS versions");
    let cfg = if verify {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder.dangerous().with_custom_certificate_verifier(Arc::new(NoVerify(provider))).with_no_client_auth()
    };
    Arc::new(cfg)
}

fn io<E: std::fmt::Display>(e: E) -> FetchError {
    FetchError::Io(e.to_string())
}

impl Connection {
    pub async fn connect(cfg: &ServerConfig) -> Result<Connection, FetchError> {
        let addr = format!("{}:{}", cfg.host, cfg.port);
        let tcp = tokio::time::timeout(Duration::from_secs(20), TcpStream::connect(&addr))
            .await
            .map_err(|_| FetchError::Io(format!("timed out connecting to {addr}")))?
            .map_err(io)?;
        let _ = tcp.set_nodelay(true);
        let stream: Box<dyn Stream> = if cfg.tls {
            let name = rustls::pki_types::ServerName::try_from(cfg.host.clone()).map_err(io)?;
            let connector = tokio_rustls::TlsConnector::from(tls_config(cfg.tls_verify));
            let tls = tokio::time::timeout(Duration::from_secs(20), connector.connect(name, tcp))
                .await
                .map_err(|_| FetchError::Io("TLS handshake timed out".into()))?
                .map_err(|e| FetchError::Io(format!("TLS: {e}")))?;
            Box::new(tls)
        } else {
            Box::new(tcp)
        };
        let mut conn = Connection { stream, buf: Vec::with_capacity(1 << 20), pos: 0 };
        let greeting = conn.read_line().await?;
        if !(greeting.starts_with("200") || greeting.starts_with("201")) {
            return Err(FetchError::Protocol(greeting));
        }
        if !cfg.username.is_empty() {
            conn.authenticate(&cfg.username, &cfg.password).await?;
        }
        Ok(conn)
    }

    async fn authenticate(&mut self, user: &str, pass: &str) -> Result<(), FetchError> {
        self.send(&format!("AUTHINFO USER {user}\r\n")).await?;
        let r = self.read_line().await?;
        if r.starts_with("281") {
            return Ok(());
        }
        if !r.starts_with("381") {
            return Err(FetchError::Auth(r));
        }
        self.send(&format!("AUTHINFO PASS {pass}\r\n")).await?;
        let r = self.read_line().await?;
        if r.starts_with("281") {
            Ok(())
        } else {
            Err(FetchError::Auth(r))
        }
    }

    async fn send(&mut self, s: &str) -> Result<(), FetchError> {
        tokio::time::timeout(IO_TIMEOUT, self.stream.write_all(s.as_bytes())).await.map_err(|_| FetchError::Io("write timed out".into()))?.map_err(io)
    }

    async fn fill(&mut self) -> Result<(), FetchError> {
        let n = tokio::time::timeout(IO_TIMEOUT, self.stream.read_buf(&mut self.buf))
            .await
            .map_err(|_| FetchError::Io("read timed out".into()))?
            .map_err(io)?;
        if n == 0 {
            return Err(FetchError::Io("connection closed by server".into()));
        }
        Ok(())
    }

    /// Drop consumed bytes. Only called between replies, when no offsets are held.
    fn compact(&mut self) {
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        } else if self.pos > (1 << 20) {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }

    async fn read_line(&mut self) -> Result<String, FetchError> {
        self.compact();
        loop {
            if let Some(i) = memchr::memmem::find(&self.buf[self.pos..], b"\r\n") {
                let line = String::from_utf8_lossy(&self.buf[self.pos..self.pos + i]).to_string();
                self.pos += i + 2;
                return Ok(line);
            }
            if self.buf.len() - self.pos > 8192 {
                return Err(FetchError::Protocol("status line too long".into()));
            }
            self.fill().await?;
        }
    }

    /// Read a multi-line block up to, and consuming, the "." terminator line.
    async fn read_block(&mut self) -> Result<Vec<u8>, FetchError> {
        self.compact();
        let start = self.pos;
        let mut scanned = start;
        loop {
            if self.buf[start..].starts_with(b".\r\n") {
                self.pos = start + 3;
                return Ok(Vec::new());
            }
            let from = scanned.saturating_sub(4).max(start);
            if let Some(i) = memchr::memmem::find(&self.buf[from..], b"\r\n.\r\n") {
                let end = from + i + 2;
                let body = self.buf[start..end].to_vec();
                self.pos = end + 3;
                return Ok(body);
            }
            scanned = self.buf.len();
            if self.buf.len() - start > 64 << 20 {
                return Err(FetchError::Protocol("article larger than 64 MiB".into()));
            }
            self.fill().await?;
        }
    }

    pub async fn send_body(&mut self, message_id: &str) -> Result<(), FetchError> {
        self.send(&format!("BODY <{message_id}>\r\n")).await
    }

    /// Read the reply to one earlier `send_body`.
    pub async fn read_body(&mut self) -> Result<Vec<u8>, FetchError> {
        let status = self.read_line().await?;
        match status.get(..3) {
            Some("222") => self.read_block().await,
            Some("430") | Some("423") | Some("420") | Some("451") => Err(FetchError::Missing),
            Some("480") => Err(FetchError::Auth(status)),
            Some("400") | Some("502") | Some("503") => Err(FetchError::Io(status)),
            _ => Err(FetchError::Protocol(status)),
        }
    }

    pub async fn body(&mut self, message_id: &str) -> Result<Vec<u8>, FetchError> {
        self.send_body(message_id).await?;
        self.read_body().await
    }

    /// Ask whether the server has an article, without fetching it.
    pub async fn stat(&mut self, message_id: &str) -> Result<bool, FetchError> {
        self.send(&format!("STAT <{message_id}>\r\n")).await?;
        let status = self.read_line().await?;
        match status.get(..3) {
            Some("223") => Ok(true),
            Some("430") | Some("423") | Some("420") | Some("451") => Ok(false),
            Some("480") => Err(FetchError::Auth(status)),
            Some("400") | Some("502") | Some("503") => Err(FetchError::Io(status)),
            _ => Err(FetchError::Protocol(status)),
        }
    }

    pub async fn quit(mut self) {
        let _ = self.send("QUIT\r\n").await;
    }
}
