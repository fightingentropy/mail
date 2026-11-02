//! TLS for IMAP and SMTP, with the same rustls and root certificates that
//! ureq uses for Google.

use std::{
    net::{TcpStream, ToSocketAddrs},
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::Context;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned, pki_types::ServerName};

pub(super) type TlsStream = StreamOwned<ClientConnection, TcpStream>;

/// Long enough for a large message on a slow connection, short enough that
/// a stalled server doesn't hold up sync for good.
const TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn tcp(host: &str, port: u16) -> anyhow::Result<TcpStream> {
    let mut last_error = None;
    for address in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, Duration::from_secs(15)) {
            Ok(stream) => {
                stream.set_read_timeout(Some(TIMEOUT))?;
                stream.set_write_timeout(Some(TIMEOUT))?;
                return Ok(stream);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.map_or_else(
        || anyhow::anyhow!("{host} has no address"),
        anyhow::Error::from,
    ))
    .with_context(|| format!("Could not reach {host}"))
}

pub(super) fn wrap(tcp: TcpStream, host: &str) -> anyhow::Result<TlsStream> {
    let name = ServerName::try_from(host.to_owned())?;
    let connection = ClientConnection::new(config(), name)?;
    Ok(StreamOwned::new(connection, tcp))
}

fn config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("ring supports the default TLS versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}
