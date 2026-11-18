pub mod codec;
pub mod proxy;
pub mod tls;

use std::collections::BTreeMap;
use std::error::Error as _;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use http::uri::PathAndQuery;
use prost_reflect::{DynamicMessage, MethodDescriptor};
use tokio::sync::mpsc;
use tonic::client::Grpc;
use tonic::codec::CompressionEncoding;
use tonic::metadata::{Ascii, MetadataKey, MetadataMap, MetadataValue};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};
use tonic::{Request, Status, Streaming};

use crate::config::{RequestConfig, ServerConfig};
use crate::status::code_name;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(7);

pub async fn connect(
    server: &ServerConfig,
    request: &RequestConfig,
) -> Result<tonic::transport::Channel> {
    match connection(server, request)? {
        Connection::Endpoint(endpoint) => endpoint
            .connect()
            .await
            .context("failed to dial to gRPC server"),
        Connection::Proxy {
            connector,
            endpoint,
        } => tonic::transport::Channel::connect(connector, endpoint)
            .await
            .context("failed to dial to gRPC server"),
    }
}

/// Creates a channel that connects lazily, like `grpc.DialContext` does.
pub fn connect_lazy(
    server: &ServerConfig,
    request: &RequestConfig,
) -> Result<tonic::transport::Channel> {
    match connection(server, request)? {
        Connection::Endpoint(endpoint) => Ok(endpoint.connect_lazy()),
        Connection::Proxy {
            connector,
            endpoint,
        } => Ok(tonic::transport::Channel::new(connector, endpoint)),
    }
}

/// The way the client connects to the server.
enum Connection {
    /// The connection goes through the connector of the endpoint of tonic, which
    /// supports the unix domain sockets.
    Endpoint(Endpoint),
    /// The connection goes through a proxy.
    Proxy {
        connector: Box<proxy::ProxyConnector>,
        endpoint: Endpoint,
    },
}

/// Builds the connection of the client.
fn connection(server: &ServerConfig, request: &RequestConfig) -> Result<Connection> {
    // A unix domain socket has neither TLS nor a proxy.
    if !server.unix_socket.is_empty() {
        if server.tls {
            anyhow::bail!(
                "the TLS connection is not supported over the unix domain socket '{}'",
                server.unix_socket
            );
        }
        let addr = format!("unix://{}", server.unix_socket);
        let endpoint = Endpoint::from_shared(addr)
            .context("failed to build the unix domain socket address")?
            .connect_timeout(CONNECT_TIMEOUT);
        return Ok(Connection::Endpoint(endpoint));
    }

    let scheme = if server.tls { "https" } else { "http" };
    let addr = format!("{scheme}://{}:{}", server.host, server.port);
    let endpoint = Endpoint::from_shared(addr)
        .context("failed to build endpoint address")?
        .connect_timeout(CONNECT_TIMEOUT);

    // The proxy needs its own connector, because the TLS of the connection is
    // negotiated over the tunnel of the proxy.
    if let Some(proxy) = proxy::parse(&server.proxy)? {
        let tls = if server.tls {
            Some(tls_params(server, request).client_config()?)
        } else {
            None
        };
        let mut connector = proxy.connector(tls)?;
        if !server.name.is_empty() {
            connector = connector.with_domain(server.name.clone());
        }
        return Ok(Connection::Proxy {
            connector: Box::new(connector),
            endpoint,
        });
    }

    Ok(Connection::Endpoint(endpoint_with_tls(
        server, request, endpoint,
    )?))
}

/// Configures the TLS of the endpoint.
fn endpoint_with_tls(
    server: &ServerConfig,
    request: &RequestConfig,
    mut endpoint: Endpoint,
) -> Result<Endpoint> {
    if server.tls {
        // The verifier of the certificate of the server is replaced when the
        // verification is turned off. The roots and the CA certificate are not set in
        // that case, because a custom verifier conflicts with them.
        let mut tls = ClientTlsConfig::new();
        if !server.insecure {
            tls = tls.with_native_roots();
            if !request.ca_cert_file.is_empty() {
                let b = std::fs::read(&request.ca_cert_file)
                    .context("failed to read the CA certificate")?;
                tls = tls.ca_certificate(Certificate::from_pem(b));
            }
        }

        if !request.cert_file.is_empty() && !request.cert_key_file.is_empty() {
            let cert = std::fs::read(&request.cert_file)
                .context("failed to read the client certificate")?;
            let key = std::fs::read(&request.cert_key_file)
                .context("failed to read the client certificate key")?;
            tls = tls.identity(Identity::from_pem(cert, key));
        }
        if !server.name.is_empty() {
            tls = tls.domain_name(server.name.clone());
        }

        endpoint = if server.insecure {
            endpoint
                .tls_config_with_verifier(tls, tls::SkipServerVerification::shared())
                .context("failed to configure TLS")?
        } else {
            endpoint
                .tls_config(tls)
                .context("failed to configure TLS")?
        };
    }

    Ok(endpoint)
}

/// The parameters of the TLS connection of the client.
fn tls_params(server: &ServerConfig, request: &RequestConfig) -> tls::TlsParams {
    tls::TlsParams {
        ca_cert_file: request.ca_cert_file.clone(),
        cert_file: request.cert_file.clone(),
        cert_key_file: request.cert_key_file.clone(),
        domain: server.name.clone(),
        insecure: server.insecure,
    }
}

/// The name of the header that carries the deadline of a call.
pub const GRPC_TIMEOUT_HEADER: &str = "grpc-timeout";

/// Parses the value of the `grpc-timeout` header, like the gRPC protocol does.
///
/// The value is a number of at most eight digits followed by the unit of the duration:
/// `H` (hours), `M` (minutes), `S` (seconds), `m` (milliseconds), `u` (microseconds)
/// or `n` (nanoseconds).
pub fn parse_grpc_timeout(value: &str) -> Result<Duration> {
    let malformed = || anyhow!("malformed grpc-timeout header: {value}");

    let Some(unit) = value.chars().last() else {
        return Err(malformed());
    };
    let digits = &value[..value.len() - unit.len_utf8()];
    if digits.is_empty() || digits.len() > 8 {
        return Err(malformed());
    }
    let amount: u64 = digits.parse().map_err(|_| malformed())?;

    let duration = match unit {
        'H' => Duration::from_secs(amount.saturating_mul(60 * 60)),
        'M' => Duration::from_secs(amount.saturating_mul(60)),
        'S' => Duration::from_secs(amount),
        'm' => Duration::from_millis(amount),
        'u' => Duration::from_micros(amount),
        'n' => Duration::from_nanos(amount),
        _ => return Err(malformed()),
    };
    Ok(duration)
}

/// Validates the deadline of the calls of the given headers.
fn validate_timeout(headers: &Headers) -> Result<()> {
    for value in headers.get(GRPC_TIMEOUT_HEADER).unwrap_or_default() {
        parse_grpc_timeout(value)?;
    }
    Ok(())
}

pub fn status_error(status: Status) -> anyhow::Error {
    anyhow!(
        "rpc error: code = {} desc = {}",
        code_name(status.code()),
        status.message()
    )
}

#[derive(Debug, Default, Clone)]
pub struct Headers(BTreeMap<String, Vec<String>>);

impl Headers {
    pub fn new(headers: BTreeMap<String, Vec<String>>) -> Self {
        Self(headers)
    }

    pub fn add(&mut self, key: &str, value: &str) -> Result<()> {
        MetadataKey::<Ascii>::from_bytes(key.as_bytes())
            .map_err(|e| anyhow!("invalid header name {key:?}: {e}"))?;
        MetadataValue::try_from(value)
            .map_err(|e| anyhow!("invalid header value {value:?}: {e}"))?;
        self.0
            .entry(key.to_string())
            .or_default()
            .push(value.to_string());
        Ok(())
    }

    pub fn remove(&mut self, key: &str) {
        self.0.remove(key);
    }

    pub fn get(&self, key: &str) -> Option<&[String]> {
        self.0.get(key).map(|v| v.as_slice())
    }

    pub fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }

    pub fn to_metadata(&self) -> Result<MetadataMap> {
        let mut map = MetadataMap::new();
        for (k, vs) in &self.0 {
            let key = MetadataKey::<Ascii>::from_bytes(k.as_bytes())
                .map_err(|e| anyhow!("invalid header name {k:?}: {e}"))?;
            for v in vs {
                let value = MetadataValue::try_from(v.as_str())
                    .map_err(|e| anyhow!("invalid header value {v:?}: {e}"))?;
                map.append(key.clone(), value);
            }
        }
        Ok(map)
    }
}

/// The result of a unary or a client streaming call.
#[derive(Debug)]
pub struct UnaryResult {
    /// The response message. It is absent when the call failed.
    pub message: Option<DynamicMessage>,
    pub headers: MetadataMap,
    pub trailers: MetadataMap,
    /// The status of the call. It is not OK when the call failed.
    pub status: Status,
}

#[derive(Debug)]
pub struct ServerStreamResult {
    pub headers: MetadataMap,
    /// The stream of the messages. It is absent when the call failed before its stream
    /// could be opened.
    pub stream: Option<Streaming<DynamicMessage>>,
    /// The status of the call. It is not OK when the call failed before its stream could
    /// be opened.
    pub status: Status,
}

#[derive(Debug)]
pub struct ClientStream {
    pub sender: mpsc::Sender<DynamicMessage>,
    pub headers: MetadataMap,
    /// The stream of the messages. It is absent when the call failed before its stream
    /// could be opened.
    pub stream: Option<Streaming<DynamicMessage>>,
    /// The status of the call. It is not OK when the call failed before its stream could
    /// be opened.
    pub status: Status,
}

#[derive(Debug, Clone)]
pub struct Client {
    grpc: Grpc<tonic::transport::Channel>,
    headers: Headers,
}

impl Client {
    pub async fn new(server: &ServerConfig, request: &RequestConfig) -> Result<Self> {
        let headers = Headers::new(request.header.clone());
        headers.to_metadata()?;
        validate_timeout(&headers)?;
        let channel = connect_lazy(server, request)?;
        let mut grpc = Grpc::new(channel);
        grpc = grpc.accept_compressed(CompressionEncoding::Gzip);
        Ok(Self { grpc, headers })
    }

    pub fn set_headers(&mut self, headers: Headers) {
        self.headers = headers;
    }

    /// Opens a call.
    ///
    /// A call that fails before its stream is opened is not an error: the status of the
    /// call is reported instead, like the original does. The headers of such a response
    /// are the ones of the status.
    async fn open<S>(&mut self, method: &MethodDescriptor, items: S) -> Result<Opened>
    where
        S: futures::Stream<Item = DynamicMessage> + Send + 'static,
    {
        let path = path_for(method)?;
        self.grpc
            .ready()
            .await
            .map_err(|e| anyhow!("failed to prepare the gRPC client: {e}"))?;
        let mut request = Request::new(items);
        *request.metadata_mut() = self.headers.to_metadata()?;
        let response = self
            .grpc
            .streaming(request, path, codec::DynamicCodec::new(method.output()))
            .await;
        match response {
            Ok(response) => {
                let headers = response.metadata().clone();
                Ok(Opened {
                    headers,
                    stream: Some(response.into_inner()),
                    status: Status::ok(""),
                })
            }
            Err(status) if status.source().is_some() => {
                // The call did not reach the server: it is a transport error.
                Err(status_error(status))
            }
            Err(status) => {
                let headers = status.metadata().clone();
                Ok(Opened {
                    headers,
                    stream: None,
                    status,
                })
            }
        }
    }

    /// Invokes a unary method. A call that fails does not return an error: its status
    /// is reported instead, like the original does.
    pub async fn invoke(
        &mut self,
        method: &MethodDescriptor,
        message: DynamicMessage,
    ) -> Result<UnaryResult> {
        let mut opened = self
            .open(method, futures::stream::once(async move { message }))
            .await?;
        let (message, trailers, status) = read_response(&mut opened).await;
        Ok(UnaryResult {
            message,
            headers: opened.headers,
            trailers,
            status,
        })
    }

    pub async fn invoke_server_streaming(
        &mut self,
        method: &MethodDescriptor,
        message: DynamicMessage,
    ) -> Result<ServerStreamResult> {
        let opened = self
            .open(method, futures::stream::once(async move { message }))
            .await?;
        Ok(ServerStreamResult {
            headers: opened.headers,
            stream: opened.stream,
            status: opened.status,
        })
    }

    /// Invokes a client streaming method. A call that fails does not return an error:
    /// its status is reported instead, like the original does.
    pub async fn invoke_client_streaming<S>(
        &mut self,
        method: &MethodDescriptor,
        requests: S,
    ) -> Result<UnaryResult>
    where
        S: futures::Stream<Item = DynamicMessage> + Send + 'static,
    {
        let mut opened = self.open(method, requests).await?;
        let (message, trailers, status) = read_response(&mut opened).await;
        Ok(UnaryResult {
            message,
            headers: opened.headers,
            trailers,
            status,
        })
    }

    pub async fn invoke_bidi_streaming(
        &mut self,
        method: &MethodDescriptor,
        buffer: usize,
    ) -> Result<ClientStream> {
        let (sender, receiver) = mpsc::channel(buffer);
        let opened = self.open(method, mpsc_to_stream(receiver)).await?;
        Ok(ClientStream {
            sender,
            headers: opened.headers,
            stream: opened.stream,
            status: opened.status,
        })
    }
}

fn path_for(method: &MethodDescriptor) -> Result<PathAndQuery> {
    format!("/{}/{}", method.parent_service().full_name(), method.name())
        .parse()
        .context("failed to build method path")
}

/// The function that renders a message of a response as it arrives.
pub type MessageRenderer<'a> = dyn FnMut(&DynamicMessage) -> Result<()> + 'a;

/// A call that has been opened.
struct Opened {
    headers: MetadataMap,
    /// The stream of the messages. It is absent when the call failed before its stream
    /// could be opened.
    stream: Option<Streaming<DynamicMessage>>,
    /// The status of the call. It is not OK when the call failed before its stream could
    /// be opened.
    status: Status,
}

/// Reads the response of an opened call.
async fn read_response(opened: &mut Opened) -> (Option<DynamicMessage>, MetadataMap, Status) {
    let Some(mut stream) = opened.stream.take() else {
        return (None, MetadataMap::new(), opened.status.clone());
    };

    let (message, status) = match stream.message().await {
        Ok(Some(message)) => (Some(message), Status::ok("")),
        Ok(None) => (None, Status::ok("")),
        Err(status) => (None, status),
    };
    let trailers = stream.trailers().await.ok().flatten().unwrap_or_default();
    (message, trailers, status)
}

/// Reads a server stream until it ends.
///
/// It returns the received messages, the trailers and the status of the call. A stream
/// that ends with an error keeps that error as its status.
pub async fn collect_stream(
    stream: Option<Streaming<DynamicMessage>>,
    status: Status,
    mut on_message: Option<&mut MessageRenderer<'_>>,
) -> Result<(Vec<DynamicMessage>, MetadataMap, Status)> {
    let Some(mut stream) = stream else {
        return Ok((Vec::new(), MetadataMap::new(), status));
    };

    let mut messages = Vec::new();
    let mut trailers = MetadataMap::new();
    let mut status = Status::ok("");

    loop {
        match stream.message().await {
            Ok(Some(msg)) => {
                // The messages are rendered as they arrive, like the original does.
                if let Some(on_message) = on_message.as_deref_mut() {
                    on_message(&msg)?;
                }
                messages.push(msg);
            }
            Ok(None) => break,
            Err(err) => {
                trailers = stream.trailers().await.ok().flatten().unwrap_or_default();
                status = err;
                break;
            }
        }
    }

    if trailers.is_empty() {
        trailers = stream.trailers().await.ok().flatten().unwrap_or_default();
    }

    Ok((messages, trailers, status))
}

fn mpsc_to_stream(
    mut receiver: mpsc::Receiver<DynamicMessage>,
) -> impl futures::Stream<Item = DynamicMessage> + Send + 'static {
    futures::stream::poll_fn(move |cx| receiver.poll_recv(cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_add_and_remove() {
        let mut headers = Headers::default();
        headers.add("x-foo", "bar").unwrap();
        headers.add("x-foo", "baz").unwrap();
        assert_eq!(
            headers.get("x-foo"),
            Some(&["bar".to_string(), "baz".to_string()][..])
        );
        assert_eq!(headers.keys(), vec!["x-foo"]);

        headers.remove("x-foo");
        assert!(headers.get("x-foo").is_none());
    }

    #[test]
    fn headers_reject_invalid_names_and_values() {
        let mut headers = Headers::default();
        assert!(headers.add("invalid header", "v").is_err());
        assert!(headers.add("x-foo", "invalid\nvalue").is_err());
    }

    #[test]
    fn headers_to_metadata_keeps_multiple_values() {
        let mut headers = Headers::default();
        headers.add("x-foo", "bar").unwrap();
        headers.add("x-foo", "baz").unwrap();
        let metadata = headers.to_metadata().unwrap();
        let values = metadata.get_all("x-foo").iter().collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn parses_the_grpc_timeout_header() {
        assert_eq!(parse_grpc_timeout("10S").unwrap(), Duration::from_secs(10));
        assert_eq!(
            parse_grpc_timeout("500m").unwrap(),
            Duration::from_millis(500)
        );
        assert_eq!(parse_grpc_timeout("1H").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_grpc_timeout("1M").unwrap(), Duration::from_secs(60));
        assert_eq!(
            parse_grpc_timeout("100u").unwrap(),
            Duration::from_micros(100)
        );
        assert_eq!(
            parse_grpc_timeout("100n").unwrap(),
            Duration::from_nanos(100)
        );
    }

    #[test]
    fn rejects_a_malformed_grpc_timeout_header() {
        for value in ["", "S", "10", "10s", "123456789S", "abc", "10S "] {
            assert!(
                parse_grpc_timeout(value).is_err(),
                "value: {value:?} should be rejected"
            );
        }
    }

    #[test]
    fn status_error_uses_grpc_wording() {
        let err = status_error(Status::not_found("no such user"));
        assert_eq!(
            err.to_string(),
            "rpc error: code = NotFound desc = no such user"
        );
    }
}
