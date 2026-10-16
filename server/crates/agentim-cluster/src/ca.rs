//! The cluster's own certificates (docs/11 §6.1). The first node creates a
//! CA; its key is stored in the database encrypted with a key derived from
//! the cluster secret. Every node issues itself a fresh certificate at
//! start, and both ends of a connection must present one from this CA.

use std::sync::Arc;

use base64::Engine;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose,
};
use ring::aead;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};

/// The name every node certificate carries; which node it is comes from
/// the handshake, checked against the node table.
pub const NODE_NAME: &str = "agentim-node";

fn ca_params(cluster_id: &str) -> CertificateParams {
    let mut p = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, format!("AgentIM cluster {cluster_id}"));
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    p
}

/// A new CA: (certificate PEM, key PEM).
pub fn new_ca(cluster_id: &str) -> Result<(String, String), String> {
    let key = KeyPair::generate().map_err(|e| e.to_string())?;
    let cert = ca_params(cluster_id)
        .self_signed(&key)
        .map_err(|e| e.to_string())?;
    Ok((cert.pem(), key.serialize_pem()))
}

fn sealing_key(secret: &str) -> aead::LessSafeKey {
    let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    let tag = ring::hmac::sign(&k, b"agentim cluster ca key v1");
    let key = aead::UnboundKey::new(&aead::AES_256_GCM, tag.as_ref()).expect("32-byte key");
    aead::LessSafeKey::new(key)
}

/// Encrypts the CA key for the database (`v1:` + base64 of nonce and
/// ciphertext); the cluster id is bound in.
pub fn seal(secret: &str, cluster_id: &str, plain: &str) -> Result<String, String> {
    let mut nonce = [0u8; 12];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce)
        .map_err(|_| "no randomness")?;
    let mut buf = plain.as_bytes().to_vec();
    sealing_key(secret)
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(cluster_id.as_bytes()),
            &mut buf,
        )
        .map_err(|_| "sealing failed")?;
    let mut all = nonce.to_vec();
    all.extend(buf);
    Ok(format!(
        "v1:{}",
        base64::engine::general_purpose::STANDARD.encode(all)
    ))
}

pub fn open(secret: &str, cluster_id: &str, sealed: &str) -> Result<String, String> {
    let raw = sealed
        .strip_prefix("v1:")
        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
        .filter(|r| r.len() > 12)
        .ok_or("the stored cluster CA key is malformed")?;
    let (nonce, ct) = raw.split_at(12);
    let mut buf = ct.to_vec();
    let plain = sealing_key(secret)
        .open_in_place(
            aead::Nonce::try_assume_unique_for_key(nonce).map_err(|_| "bad nonce")?,
            aead::Aad::from(cluster_id.as_bytes()),
            &mut buf,
        )
        .map_err(|_| {
            "the cluster secret does not open the stored CA key (every node needs the same cluster.secret or master key)"
        })?;
    String::from_utf8(plain.to_vec()).map_err(|e| e.to_string())
}

/// This node's TLS material, issued by the cluster CA.
pub struct Tls {
    pub server: Arc<rustls::ServerConfig>,
    pub client: Arc<rustls::ClientConfig>,
}

impl Tls {
    pub fn new(
        cluster_id: &str,
        ca_pem: &str,
        ca_key_pem: &str,
        node_id: &str,
    ) -> Result<Tls, String> {
        let ca_key = KeyPair::from_pem(ca_key_pem).map_err(|e| e.to_string())?;
        let issuer = Issuer::new(ca_params(cluster_id), ca_key);
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let mut p =
            CertificateParams::new(vec![NODE_NAME.to_string()]).map_err(|e| e.to_string())?;
        p.distinguished_name.push(DnType::CommonName, node_id);
        p.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let cert = p.signed_by(&key, &issuer).map_err(|e| e.to_string())?;
        let ca_der: CertificateDer<'static> = {
            use rustls::pki_types::pem::PemObject;
            CertificateDer::from_pem_slice(ca_pem.as_bytes()).map_err(|e| e.to_string())?
        };
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca_der).map_err(|e| e.to_string())?;
        let roots = Arc::new(roots);
        let chain = vec![cert.der().clone()];
        let pk = || PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            roots.clone(),
            provider.clone(),
        )
        .build()
        .map_err(|e| e.to_string())?;
        let server = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| e.to_string())?
            .with_client_cert_verifier(verifier)
            .with_single_cert(chain.clone(), pk())
            .map_err(|e| e.to_string())?;
        let client = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_client_auth_cert(chain, pk())
            .map_err(|e| e.to_string())?;
        Ok(Tls {
            server: Arc::new(server),
            client: Arc::new(client),
        })
    }
}

pub fn server_name() -> ServerName<'static> {
    ServerName::try_from(NODE_NAME).expect("valid name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ca_key_is_sealed_to_the_secret_and_cluster() {
        let s = seal("secret", "c1", "key").unwrap();
        assert!(s.starts_with("v1:") && !s.contains("key"));
        assert_eq!(open("secret", "c1", &s).unwrap(), "key");
        assert_ne!(seal("secret", "c1", "key").unwrap(), s, "fresh nonce");
        assert!(open("other", "c1", &s).is_err());
        assert!(open("secret", "c2", &s).is_err());
        assert!(open("secret", "c1", "v1:AAAA").is_err());
        assert!(open("secret", "c1", "plain").is_err());
    }

    #[test]
    fn node_certificates_come_from_the_ca() {
        let (ca, key) = new_ca("c1").unwrap();
        assert!(Tls::new("c1", &ca, &key, "a").is_ok());
        assert!(Tls::new("c1", &ca, "not a key", "a").is_err());
        assert!(Tls::new("c1", "not a cert", &key, "a").is_err());
    }
}
