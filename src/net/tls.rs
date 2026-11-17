//! TLS for `https://` addresses: a network folder on a NAS that is reached over the
//! internet or that speaks nothing else, and the lyrics service where that is switched
//! on.
//!
//! A server is believed when its certificate goes back to one of the authorities
//! Mozilla trusts (they are compiled in: the app looks the same on every system), and
//! its name is the one asked for. A NAS at home often has a certificate it made
//! itself, which no authority stands behind. Such a server is refused, and the refusal
//! carries the certificate's fingerprint ([`NetError::Untrusted`]); when the listener
//! has looked at it and said that this is their server, that one certificate is
//! trusted from then on (`pinned`), and no other: a server that shows another one
//! later is refused again.
use super::NetError;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme,
    StreamOwned,
};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// A connection that speaks TLS.
pub(super) type Secure = StreamOwned<ClientConnection, TcpStream>;

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    static PROVIDER: LazyLock<Arc<rustls::crypto::CryptoProvider>> =
        LazyLock::new(|| Arc::new(rustls::crypto::ring::default_provider()));
    PROVIDER.clone()
}

/// The check every certificate gets: an authority Mozilla trusts stands behind it.
fn usual() -> Option<Arc<WebPkiServerVerifier>> {
    static USUAL: LazyLock<Option<Arc<WebPkiServerVerifier>>> = LazyLock::new(|| {
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
            .build()
            .ok()
    });
    USUAL.clone()
}

/// The SHA-256 of a certificate as it is shown and kept: 64 hexadecimal digits.
pub fn fingerprint(certificate: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, certificate)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A fingerprint as a person compares it: upper case, in groups of four.
pub fn spelled(fingerprint: &str) -> String {
    let upper = fingerprint.to_ascii_uppercase();
    upper
        .as_bytes()
        .chunks(4)
        .map(|group| String::from_utf8_lossy(group).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The usual check, with the one exception the listener made.
#[derive(Debug)]
struct Verifier {
    usual: Arc<WebPkiServerVerifier>,
    /// The fingerprint of the one certificate trusted without an authority behind it.
    pinned: Option<String>,
    /// The fingerprint of the certificate the server showed.
    shown: Arc<Mutex<Option<String>>>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let shown = fingerprint(end_entity.as_ref());
        let trusted = self.pinned.as_deref().map(|pinned| pinned == shown);
        *self.shown.lock().unwrap_or_else(|p| p.into_inner()) = Some(shown);
        match trusted {
            // (That the server holds this certificate's key is still checked: the
            // handshake's signature is verified below like any other.)
            Some(true) => Ok(ServerCertVerified::assertion()),
            // Who trusts one certificate of a server trusts no other from it, whoever
            // vouches for that one: the listener was told so.
            Some(false) => Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            )),
            None => self.usual.verify_server_cert(
                end_entity,
                intermediates,
                server_name,
                ocsp_response,
                now,
            ),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.usual.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.usual.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.usual.supported_verify_schemes()
    }
}

/// What a handshake fails with when what answers is not TLS at all (a port that
/// speaks plain HTTP, mostly).
pub const NOT_TLS: &str = "the server does not speak TLS";

fn failed(what: &'static str) -> NetError {
    NetError::BadResponse(what)
}

/// Whether a failed handshake failed at the server's certificate.
fn about_the_certificate(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .is_some_and(|error| matches!(error, rustls::Error::InvalidCertificate(_)))
}

/// Speaks TLS on a connection that was just made to `host`, to the end of the
/// handshake: what comes back is a server that showed a certificate that is believed.
/// `pinned`: the fingerprint of a certificate that is trusted as it is.
/// `within`: how long the whole handshake may take. Each read of it has the
/// socket's own timeout already, which a server that sends a byte now and then
/// never runs into.
pub(super) fn connect(
    host: &str,
    mut stream: TcpStream,
    pinned: Option<&str>,
    within: Duration,
) -> Result<Secure, NetError> {
    let name = ServerName::try_from(host.to_owned()).map_err(|_| NetError::BadAddress)?;
    let shown = Arc::new(Mutex::new(None));
    let verifier = Verifier {
        usual: usual().ok_or(failed("no certificate authorities"))?,
        pinned: pinned.map(str::to_ascii_lowercase),
        shown: Arc::clone(&shown),
    };
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|_| failed("TLS cannot be set up"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let mut connection =
        ClientConnection::new(Arc::new(config), name).map_err(|_| NetError::BadAddress)?;
    let usual_timeout = stream.read_timeout().ok().flatten();
    let mut timed = Timed {
        stream: &mut stream,
        until: Instant::now() + within,
    };
    while connection.is_handshaking() {
        if let Err(error) = connection.complete_io(&mut timed) {
            let shown = shown.lock().unwrap_or_else(|p| p.into_inner()).take();
            let pinned = pinned.map(str::to_ascii_lowercase);
            return Err(match shown {
                // The certificate is the one that is trusted, and still it cannot be
                // used: of a kind that is not checked here (a key too short, a
                // certificate of the first version). Trusting it once more would
                // change nothing.
                Some(fingerprint)
                    if about_the_certificate(&error)
                        && pinned.as_deref() == Some(fingerprint.as_str()) =>
                {
                    failed("the server's certificate is of a kind that cannot be checked")
                }
                Some(fingerprint) if about_the_certificate(&error) => {
                    NetError::Untrusted { fingerprint }
                }
                _ => match error.kind() {
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                        NetError::Timeout
                    }
                    std::io::ErrorKind::InvalidData => failed(NOT_TLS),
                    _ => NetError::Io(error),
                },
            });
        }
    }
    // (The reads that follow have the timeout the socket came with.)
    stream
        .set_read_timeout(usual_timeout)
        .map_err(NetError::Io)?;
    Ok(StreamOwned::new(connection, stream))
}

/// A connection during the handshake: a read gives up when the time for the whole
/// handshake is over.
struct Timed<'a> {
    stream: &'a mut TcpStream,
    until: Instant,
}

impl Read for Timed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self
            .until
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or(std::io::ErrorKind::TimedOut)?;
        // Not longer than the socket's own timeout, where it has one.
        let timeout = match self.stream.read_timeout()? {
            Some(usual) => usual.min(left),
            None => left,
        };
        self.stream.set_read_timeout(Some(timeout))?;
        self.stream.read(buf)
    }
}

impl Write for Timed<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fingerprint_is_the_sha_256_in_hex() {
        assert_eq!(
            fingerprint(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            fingerprint(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(spelled("ba7816bf8f01"), "BA78 16BF 8F01");
        assert_eq!(spelled(""), "");
    }

    #[test]
    fn the_authorities_are_there() {
        assert!(usual().is_some());
        assert!(webpki_roots::TLS_SERVER_ROOTS.len() > 50);
    }
}
