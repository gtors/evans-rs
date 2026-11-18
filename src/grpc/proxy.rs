//! The proxy support of the client.
//!
//! The connection to the server goes through the proxy, which tunnels the TCP
//! stream to the server. The TLS handshake happens over the tunnel, so the
//! certificate of the server is still verified against the server itself.

use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::{Context as _, Result, anyhow};
use futures::future::BoxFuture;
use http::Uri;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::connect::proxy::{SocksV4, SocksV5, Tunnel};
use hyper_util::rt::TokioIo;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tower_service::Service;

/// A stream that can be read and written.
pub trait AsyncReadWrite: AsyncRead + AsyncWrite {}

impl<T: AsyncRead + AsyncWrite> AsyncReadWrite for T {}

/// The stream of a connection, boxed so that the connectors of the different
/// transports have the same type.
pub type BoxIo = Box<dyn AsyncReadWrite + Send + Unpin>;

/// The errors of the proxy connectors.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    /// The proxy could not be dialed.
    #[error("failed to connect through the proxy: {0}")]
    Dial(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The TLS handshake with the server failed.
    #[error("failed to establish the TLS connection: {0}")]
    Tls(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The name of the server is invalid.
    #[error("invalid server name '{0}'")]
    InvalidServerName(String),
    /// The host of the server is missing.
    #[error("failed to resolve the host of the server")]
    MissingHost,
}

/// The kind of the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    /// A proxy that tunnels the connection with the HTTP CONNECT method.
    Http,
    /// A SOCKS5 proxy.
    Socks5,
    /// A SOCKS4 proxy.
    Socks4,
}

impl ProxyKind {
    /// Returns the kind of the proxy of the given address.
    pub fn of(address: &Uri) -> Result<Self> {
        match address.scheme_str() {
            Some("http") | Some("https") => Ok(Self::Http),
            Some("socks5") | Some("socks5h") => Ok(Self::Socks5),
            Some("socks4") | Some("socks4a") => Ok(Self::Socks4),
            _ => Err(anyhow!(
                "unknown proxy protocol '{}'. the supported protocols are http, socks5 and socks4",
                address.scheme_str().unwrap_or_default()
            )),
        }
    }
}

/// The settings of the proxy.
#[derive(Debug, Clone)]
pub struct Proxy {
    /// The kind of the proxy.
    kind: ProxyKind,
    /// The address of the proxy.
    address: Uri,
    /// The user of the proxy, if it needs the authentication.
    user: Option<String>,
    /// The password of the user of the proxy.
    pass: Option<String>,
}

impl Proxy {
    /// Parses the address of the proxy.
    ///
    /// The credentials may be part of the address, like
    /// `socks5://user:password@127.0.0.1:1080`.
    pub fn parse(address: &str) -> Result<Self> {
        // The credentials of the proxy are part of its address, like
        // `socks5://user:password@127.0.0.1:1080`. The URI of the http crate does
        // not expose them, so they are split off here.
        let (scheme, rest) = address
            .split_once("://")
            .ok_or_else(|| anyhow!("invalid proxy address '{address}': the scheme is missing"))?;
        let (credentials, host) = match rest.rsplit_once('@') {
            Some((credentials, host)) => (Some(credentials), host),
            None => (None, rest),
        };

        let uri: Uri = format!("{scheme}://{host}")
            .parse()
            .map_err(|e| anyhow!("invalid proxy address '{address}': {e}"))?;

        let kind = ProxyKind::of(&uri)?;

        let (user, pass) = match credentials {
            Some(credentials) => match credentials.split_once(':') {
                Some((user, pass)) => (Some(user.to_string()), Some(pass.to_string())),
                None => (Some(credentials.to_string()), None),
            },
            None => (None, None),
        };

        // The connectors of the proxy dial it with a plain TCP connection, so its
        // address always has the http scheme.
        let address = format!("http://{host}")
            .parse()
            .map_err(|e| anyhow!("invalid proxy address '{address}': {e}"))?;

        Ok(Self {
            kind,
            address,
            user,
            pass,
        })
    }

    /// Returns the kind of the proxy.
    pub fn kind(&self) -> ProxyKind {
        self.kind
    }

    /// Returns the address of the proxy.
    pub fn address(&self) -> &Uri {
        &self.address
    }

    /// Builds the connector that dials the server through the proxy.
    ///
    /// The TLS configuration is optional: it is used when the connection to the
    /// server is encrypted.
    pub fn connector(&self, tls: Option<Arc<ClientConfig>>) -> Result<ProxyConnector> {
        let mut tcp = HttpConnector::new();
        tcp.set_connect_timeout(Some(super::CONNECT_TIMEOUT));

        let inner = match self.kind {
            ProxyKind::Http => {
                let tunnel = Tunnel::new(self.address.clone(), tcp);
                match (&self.user, &self.pass) {
                    (Some(user), Some(pass)) => {
                        Inner::Http(tunnel.with_auth(auth_header(user, pass)?))
                    }
                    _ => Inner::Http(tunnel),
                }
            }
            ProxyKind::Socks4 => Inner::Socks4(SocksV4::new(self.address.clone(), tcp)),
            ProxyKind::Socks5 => {
                let socks = SocksV5::new(self.address.clone(), tcp);
                match (&self.user, &self.pass) {
                    (Some(user), Some(pass)) => {
                        Inner::Socks5(socks.with_auth(user.clone(), pass.clone()))
                    }
                    _ => Inner::Socks5(socks),
                }
            }
        };

        Ok(ProxyConnector {
            inner,
            tls,
            domain: None,
        })
    }
}

/// Builds the value of the `Proxy-Authorization` header.
fn auth_header(user: &str, pass: &str) -> Result<http::HeaderValue> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;

    let credentials = BASE64.encode(format!("{user}:{pass}"));
    let mut value = http::HeaderValue::from_str(&format!("Basic {credentials}"))
        .map_err(|e| anyhow!("failed to build the proxy credentials: {e}"))?;
    value.set_sensitive(true);
    Ok(value)
}

/// The connector of the proxy.
///
/// The variants of the inner enum wrap the same TCP connector, so they have the same
/// response type.
enum Inner {
    /// A proxy that tunnels the connection with the HTTP CONNECT method.
    Http(Tunnel<HttpConnector>),
    /// A SOCKS4 proxy.
    Socks4(SocksV4<HttpConnector>),
    /// A SOCKS5 proxy.
    Socks5(SocksV5<HttpConnector>),
}

/// The connector that dials the server through the proxy.
pub struct ProxyConnector {
    /// The connector of the proxy.
    inner: Inner,
    /// The configuration of the TLS client, if the connection is encrypted.
    tls: Option<Arc<ClientConfig>>,
    /// The name that the certificate of the server is verified against.
    domain: Option<String>,
}

impl ProxyConnector {
    /// Sets the name that the certificate of the server is verified against.
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// Returns the name that the certificate of the server is verified against.
    fn domain(&self, dst: &Uri) -> Result<String, ProxyError> {
        match &self.domain {
            Some(domain) => Ok(domain.clone()),
            None => dst
                .host()
                .map(|host| host.trim_matches(['[', ']']).to_string())
                .ok_or(ProxyError::MissingHost),
        }
    }
}

impl Service<Uri> for ProxyConnector {
    type Response = TokioIo<BoxIo>;
    type Error = ProxyError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match &mut self.inner {
            Inner::Http(connector) => connector
                .poll_ready(cx)
                .map_err(|e| ProxyError::Dial(Box::new(e))),
            Inner::Socks4(connector) => connector
                .poll_ready(cx)
                .map_err(|e| ProxyError::Dial(Box::new(e))),
            Inner::Socks5(connector) => connector
                .poll_ready(cx)
                .map_err(|e| ProxyError::Dial(Box::new(e))),
        }
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let tls = self.tls.clone();
        let domain = match self.domain(&dst) {
            Ok(domain) => domain,
            Err(err) => {
                return Box::pin(async move { Err(err) });
            }
        };

        match &mut self.inner {
            Inner::Http(connector) => {
                let connecting = connector.call(dst.clone());
                Box::pin(async move {
                    let io = connecting
                        .await
                        .map_err(|e| ProxyError::Dial(Box::new(e)))?;
                    finish(io.into_inner(), tls, domain).await
                })
            }
            Inner::Socks4(connector) => {
                let connecting = connector.call(dst.clone());
                Box::pin(async move {
                    let io = connecting
                        .await
                        .map_err(|e| ProxyError::Dial(Box::new(e)))?;
                    finish(io.into_inner(), tls, domain).await
                })
            }
            Inner::Socks5(connector) => {
                let connecting = connector.call(dst);
                Box::pin(async move {
                    let io = connecting
                        .await
                        .map_err(|e| ProxyError::Dial(Box::new(e)))?;
                    finish(io.into_inner(), tls, domain).await
                })
            }
        }
    }
}

/// Encrypts the stream of the proxy, if the connection is a TLS one.
async fn finish(
    io: TcpStream,
    tls: Option<Arc<ClientConfig>>,
    domain: String,
) -> Result<TokioIo<BoxIo>, ProxyError> {
    let io: BoxIo = Box::new(io);

    let Some(tls) = tls else {
        return Ok(TokioIo::new(io));
    };

    let name = ServerName::try_from(domain.clone())
        .map_err(|_| ProxyError::InvalidServerName(domain.clone()))?;

    let stream = tokio_rustls::TlsConnector::from(tls)
        .connect(name, io)
        .await
        .map_err(|e| ProxyError::Tls(Box::new(e)))?;

    Ok(TokioIo::new(Box::new(stream) as BoxIo))
}

/// Reads the address of the proxy from the environment, if there is one.
pub fn from_env() -> Option<String> {
    for name in ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy"] {
        if let Ok(value) = std::env::var(name)
            && !value.is_empty()
        {
            return Some(value);
        }
    }
    None
}

/// Parses the address of the proxy, falling back to the environment.
pub fn parse(address: &str) -> Result<Option<Proxy>> {
    let address = if address.is_empty() {
        match from_env() {
            Some(address) => address,
            None => return Ok(None),
        }
    } else {
        address.to_string()
    };

    let proxy = Proxy::parse(&address).context("failed to parse the proxy address")?;
    Ok(Some(proxy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_address_of_the_proxy() {
        let proxy = Proxy::parse("socks5://127.0.0.1:1080").unwrap();
        assert_eq!(proxy.kind(), ProxyKind::Socks5);
        assert_eq!(proxy.address().to_string(), "http://127.0.0.1:1080/");
        assert_eq!(proxy.user, None);

        let proxy = Proxy::parse("http://user:password@proxy.example.com:8080").unwrap();
        assert_eq!(proxy.kind(), ProxyKind::Http);
        assert_eq!(
            proxy.address().to_string(),
            "http://proxy.example.com:8080/"
        );
        assert_eq!(proxy.user.as_deref(), Some("user"));
        assert_eq!(proxy.pass.as_deref(), Some("password"));

        let proxy = Proxy::parse("socks4://127.0.0.1:1080").unwrap();
        assert_eq!(proxy.kind(), ProxyKind::Socks4);
    }

    #[test]
    fn rejects_an_unknown_proxy_protocol() {
        let err = Proxy::parse("ftp://127.0.0.1:21").unwrap_err();
        assert!(err.to_string().contains("unknown proxy protocol"), "{err}");
    }

    #[test]
    fn rejects_an_address_without_a_scheme() {
        let err = Proxy::parse("127.0.0.1:1080").unwrap_err();
        assert!(err.to_string().contains("the scheme is missing"), "{err}");
    }
}
