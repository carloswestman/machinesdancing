//! barre's certificate authority: it signs the certificates barre shows a VM
//! for names whose rule gives a credential, so barre can read and change
//! those requests. The guest image trusts its certificate; its key never
//! leaves the host.
//!
//! `barre ca init` makes it once, in `/etc/barre`: `ca.key`, readable by
//! root and barre's group, and `ca.crt`, public.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose,
    IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use time::OffsetDateTime;

/// Where the certificate authority lives.
pub const DIR: &str = "/etc/barre";

const NAME: &str = "barre certificate authority";
/// How long a name's certificate is used before a new one is made.
const LEAF_REUSE: Duration = Duration::from_secs(24 * 3600);

/// The authority's parameters. The issuer is rebuilt from them and the key,
/// so they must stay the same as those its certificate was made with.
fn params() -> CertificateParams {
    let mut p = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, NAME);
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    p
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// Makes the authority in `dir`, unless it has one. Returns whether it made
/// one. Files take the group of `dir`.
pub fn init(dir: &Path) -> io::Result<bool> {
    let (key_path, crt_path) = (dir.join("ca.key"), dir.join("ca.crt"));
    if key_path.exists() {
        return Ok(false);
    }
    let gid = fs::metadata(dir)?.gid();
    let key = KeyPair::generate().map_err(other)?;
    let mut p = params();
    let now = OffsetDateTime::now_utc();
    p.not_before = now - time::Duration::days(1);
    p.not_after = now + time::Duration::days(3650);
    let cert = p.self_signed(&key).map_err(other)?;

    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .open(&key_path)?;
    std::os::unix::fs::fchown(&f, None, Some(gid))?;
    f.write_all(key.serialize_pem().as_bytes())?;
    f.sync_all()?;
    let tmp = dir.join(".ca.crt.tmp");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&tmp)?;
    f.write_all(cert.pem().as_bytes())?;
    f.sync_all()?;
    fs::rename(tmp, crt_path)?;
    Ok(true)
}

pub struct Ca {
    issuer: Issuer<'static, KeyPair>,
    cert: CertificateDer<'static>,
    /// A server configuration per name, and when it was made.
    leaves: Mutex<HashMap<String, (Instant, Arc<ServerConfig>)>>,
}

impl Ca {
    pub fn load(dir: &Path) -> io::Result<Ca> {
        let key = fs::read_to_string(dir.join("ca.key"))?;
        let key = KeyPair::from_pem(&key).map_err(other)?;
        let cert = CertificateDer::from_pem_file(dir.join("ca.crt")).map_err(other)?;
        Ok(Ca {
            issuer: Issuer::new(params(), key),
            cert,
            leaves: Mutex::new(HashMap::new()),
        })
    }

    /// A TLS server configuration presenting a certificate for `name`,
    /// signed by the authority, speaking HTTP/1.1 only.
    pub fn server_config(&self, name: &str) -> Result<Arc<ServerConfig>, String> {
        let mut leaves = self.leaves.lock().unwrap();
        if let Some((made, config)) = leaves.get(name)
            && made.elapsed() < LEAF_REUSE
        {
            return Ok(config.clone());
        }
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let mut p = CertificateParams::new(vec![name.to_string()]).map_err(|e| e.to_string())?;
        let now = OffsetDateTime::now_utc();
        p.not_before = now - time::Duration::hours(1);
        p.not_after = now + time::Duration::days(7);
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.use_authority_key_identifier_extension = true;
        let cert = p.signed_by(&key, &self.issuer).map_err(|e| e.to_string())?;
        let chain = vec![cert.der().clone(), self.cert.clone()];
        let key = PrivateKeyDer::try_from(key.serialize_der()).map_err(|e| e.to_string())?;
        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|e| e.to_string())?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let config = Arc::new(config);
        leaves.insert(name.to_string(), (Instant::now(), config.clone()));
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_authority_is_made_once_and_signs_names() {
        let dir = std::env::temp_dir().join(format!("barre-ca-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(init(&dir).unwrap());
        let key = fs::read(dir.join("ca.key")).unwrap();
        assert!(!init(&dir).unwrap());
        assert_eq!(fs::read(dir.join("ca.key")).unwrap(), key);
        assert_eq!(fs::metadata(dir.join("ca.key")).unwrap().mode() & 0o777, 0o640);

        let ca = Ca::load(&dir).unwrap();
        let a = ca.server_config("api.example.com").unwrap();
        let b = ca.server_config("api.example.com").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.alpn_protocols, vec![b"http/1.1".to_vec()]);
        fs::remove_dir_all(dir).unwrap();
    }
}
