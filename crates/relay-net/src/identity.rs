//! A device's long-lived identity: an ECDSA key and a self-signed certificate.
//! The device ID is derived from the public key, so it survives certificate
//! regeneration.

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use std::path::Path;

pub struct Identity {
    pub id: String,
    pub cert: CertificateDer<'static>,
    pub key_der: Vec<u8>,
}

impl Identity {
    pub fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone()))
    }
}

pub fn load_or_create(dir: &Path) -> Result<Identity> {
    let path = dir.join("identity.key");
    let key = match std::fs::read_to_string(&path) {
        Ok(pem) => rcgen::KeyPair::from_pem(&pem).context("identity key unreadable")?,
        Err(_) => {
            let key = rcgen::KeyPair::generate()?;
            std::fs::create_dir_all(dir)?;
            std::fs::write(&path, key.serialize_pem())?;
            key
        }
    };
    let params = rcgen::CertificateParams::new(vec!["relay".to_string()])?;
    let cert = params.self_signed(&key)?;
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let id = id_from_cert(&cert_der).context("own certificate unparsable")?;
    Ok(Identity { id, cert: cert_der, key_der: key.serialize_der() })
}

/// Reads one DER TLV, returning (tag, content, whole element, rest).
fn tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8], &[u8])> {
    let tag = *b.first()?;
    let first = *b.get(1)? as usize;
    let (len, hdr) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *b.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let end = hdr.checked_add(len)?;
    if end > b.len() {
        return None;
    }
    Some((tag, &b[hdr..end], &b[..end], &b[end..]))
}

/// SHA-256 of the certificate's SubjectPublicKeyInfo, first 8 bytes in hex.
pub fn id_from_cert(cert: &[u8]) -> Option<String> {
    let (_, cert_body, _, _) = tlv(cert)?;
    let (_, tbs, _, _) = tlv(cert_body)?;
    let mut rest = tbs;
    // Optional explicit version [0].
    if rest.first() == Some(&0xA0) {
        rest = tlv(rest)?.3;
    }
    // serial, signature, issuer, validity, subject
    for _ in 0..5 {
        rest = tlv(rest)?.3;
    }
    let (_, _, spki, _) = tlv(rest)?;
    let digest = Sha256::digest(spki);
    Some(hex::encode(&digest[..8]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_across_certificates() {
        let dir = std::env::temp_dir().join(format!("relay-id-test-{}", std::process::id()));
        let a = load_or_create(&dir).unwrap();
        let b = load_or_create(&dir).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.id.len(), 16);
        let _ = std::fs::remove_dir_all(dir);
    }
}
