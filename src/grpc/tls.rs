//! The TLS settings of the client.

use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

/// The application protocol of gRPC over HTTP/2.
const ALPN_H2: &[u8] = b"h2";

/// A verifier that accepts any certificate of the server.
///
/// It is used by the `--insecure` option, which turns the verification of the
/// certificate of the server off. The connection is still encrypted, but it is not
/// authenticated, so it is vulnerable to a man-in-the-middle attack.
#[derive(Debug)]
pub struct SkipServerVerification {
    /// The signature schemes that the client supports.
    schemes: Vec<SignatureScheme>,
}

impl SkipServerVerification {
    /// Creates a verifier that accepts any certificate.
    pub fn new() -> Self {
        let schemes = match CryptoProvider::get_default() {
            Some(provider) => provider
                .signature_verification_algorithms
                .supported_schemes(),
            None => rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes(),
        };

        Self { schemes }
    }

    /// Returns the verifier as a trait object.
    pub fn shared() -> Arc<dyn ServerCertVerifier> {
        Arc::new(Self::new())
    }
}

impl Default for SkipServerVerification {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes.clone()
    }
}

/// The parameters of the TLS connection of the client.
#[derive(Debug, Clone, Default)]
pub struct TlsParams {
    /// The file of the CA certificate that verifies the certificate of the server.
    pub ca_cert_file: String,
    /// The file of the certificate of the client.
    pub cert_file: String,
    /// The file of the key of the certificate of the client.
    pub cert_key_file: String,
    /// The name that the certificate of the server is verified against.
    pub domain: String,
    /// The certificate of the server is not verified.
    pub insecure: bool,
}

impl TlsParams {
    /// Builds the configuration of the TLS client.
    ///
    /// It mirrors what `ClientTlsConfig` of tonic does, so that the connections that
    /// go through a proxy behave like the direct ones.
    pub fn client_config(&self) -> Result<Arc<ClientConfig>> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("failed to build the TLS configuration")?;

        let mut config = if self.insecure {
            builder
                .dangerous()
                .with_custom_certificate_verifier(SkipServerVerification::shared())
                .with_no_client_auth()
        } else {
            let mut roots = RootCertStore::empty();
            let native = rustls_native_certs::load_native_certs();
            for cert in native.certs {
                roots
                    .add(cert)
                    .context("failed to add a native CA certificate")?;
            }
            if !self.ca_cert_file.is_empty() {
                let pem = std::fs::read(&self.ca_cert_file)
                    .context("failed to read the CA certificate")?;
                for cert in parse_certificates(&pem)? {
                    roots
                        .add(cert)
                        .context("failed to add the CA certificate")?;
                }
            }

            let builder = builder.with_root_certificates(roots);
            match self.identity()? {
                Some((certs, key)) => builder
                    .with_client_auth_cert(certs, key)
                    .context("failed to configure the client certificate")?,
                None => builder.with_no_client_auth(),
            }
        };

        config.alpn_protocols = vec![ALPN_H2.to_vec()];
        Ok(Arc::new(config))
    }

    /// Builds the certificate and the key of the client, if it has one.
    fn identity(&self) -> Result<Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>> {
        if self.cert_file.is_empty() || self.cert_key_file.is_empty() {
            return Ok(None);
        }

        let cert =
            std::fs::read(&self.cert_file).context("failed to read the client certificate")?;
        let key = std::fs::read(&self.cert_key_file)
            .context("failed to read the client certificate key")?;

        use rustls::pki_types::pem::PemObject;

        let certs = parse_certificates(&cert)?;
        let key = PrivateKeyDer::from_pem_slice(&key)
            .context("failed to parse the key of the client certificate")?;

        Ok(Some((certs, key)))
    }
}

/// Parses the PEM encoded certificates.
fn parse_certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject;

    CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to parse the PEM encoded certificates")
}
