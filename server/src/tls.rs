//! Built-in TLS (docs/04-security-compliance.md §1): HTTPS and WSS without a
//! proxy. Handshakes run off the accept loop with a timeout, so a slow or
//! silent client cannot hold up new connections.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Loads a PEM certificate chain and private key.
pub fn server_config(cert: &str, key: &str) -> Result<rustls::ServerConfig, String> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| format!("{cert}: {e}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{cert}: {e}"))?;
    if certs.is_empty() {
        return Err(format!("{cert}: no certificate"));
    }
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("{key}: {e}"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("certificate and key do not match: {e}"))?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(cfg)
}

/// The client address of a TLS connection (the open API's address rules
/// read it as `ConnectInfo<PeerAddr>`; plain TCP gives `ConnectInfo<SocketAddr>`).
#[derive(Clone, Copy, Debug)]
pub struct PeerAddr(pub SocketAddr);

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, TlsListener>>
    for PeerAddr
{
    fn connect_info(s: axum::serve::IncomingStream<'_, TlsListener>) -> PeerAddr {
        PeerAddr(*s.remote_addr())
    }
}

/// A listener yielding connections that completed the TLS handshake.
pub struct TlsListener {
    rx: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
}

impl TlsListener {
    pub fn new(tcp: TcpListener, cfg: rustls::ServerConfig) -> std::io::Result<TlsListener> {
        let local = tcp.local_addr()?;
        let acceptor = TlsAcceptor::from(Arc::new(cfg));
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            loop {
                let (sock, addr) = match tcp.accept().await {
                    Ok(x) => x,
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let _ = sock.set_nodelay(true);
                let (acceptor, tx) = (acceptor.clone(), tx.clone());
                tokio::spawn(async move {
                    if let Ok(Ok(s)) =
                        tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(sock)).await
                    {
                        let _ = tx.send((s, addr)).await;
                    }
                });
            }
        });
        Ok(TlsListener { rx, local })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(x) => x,
            // The accept task never ends while the listener lives.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local)
    }
}
