//! The signing key of signed images: an RSA key and its self-signed
//! certificate, kept outside the repository and created on first use.

use super::der;
use anyhow::{Context, Result, bail};
use rsa::PrivateKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const KEY_FILE: &str = "mikros.key";
pub const CERT_FILE: &str = "mikros.crt";
const SUBJECT: &str = "Mikros Secure Boot";
const BITS: usize = 2048;
const VALIDITY_YEARS: u64 = 10;

pub const SHA256_WITH_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 11];
pub const RSA_ENCRYPTION: &[u64] = &[1, 2, 840, 113549, 1, 1, 1];
pub const SHA256: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 1];

/// The AlgorithmIdentifier of `arcs` with NULL parameters.
pub fn algorithm(arcs: &[u64]) -> Vec<u8> {
    der::seq(&[&der::oid(arcs), &der::null()])
}

/// A loaded signing key.
pub struct Key {
    private: PrivateKey,
    /// The DER encoding of the certificate.
    pub cert: Vec<u8>,
    /// The DER encodings of the certificate's issuer and serial number,
    /// which name it in signatures.
    pub issuer: Vec<u8>,
    pub serial: Vec<u8>,
}

impl Key {
    /// The PKCS #1 v1.5 signature of the SHA-256 digest of `data`.
    pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>> {
        Ok(self.private.pkcs1v15_sign::<Sha256>(data)?)
    }
}

/// The directory that holds the key when `--key` is not given.
pub fn default_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share/mikros/secure-boot"))
}

/// Loads the key in `dir`, or creates one there when it holds none.
pub fn load_or_create(dir: &Path) -> Result<Key> {
    let key_path = dir.join(KEY_FILE);
    let cert_path = dir.join(CERT_FILE);
    if !key_path.exists() && !cert_path.exists() {
        create(dir)?;
        println!(
            "created the signing key {} and its certificate {}",
            key_path.display(),
            cert_path.display()
        );
    }
    load(dir)
}

fn load(dir: &Path) -> Result<Key> {
    let key_path = dir.join(KEY_FILE);
    let cert_path = dir.join(CERT_FILE);
    let pem = fs::read_to_string(&key_path)
        .with_context(|| format!("reading the signing key {}", key_path.display()))?;
    let private = PrivateKey::from_pkcs8_pem(&pem)
        .map_err(|e| anyhow::anyhow!("{} is not a PKCS #8 RSA key: {e}", key_path.display()))?;
    let pem = fs::read(&cert_path)
        .with_context(|| format!("reading the certificate {}", cert_path.display()))?;
    let (label, cert) = pem_rfc7468::decode_vec(&pem)
        .map_err(|e| anyhow::anyhow!("{} is not PEM: {e}", cert_path.display()))?;
    if label != "CERTIFICATE" {
        bail!("{} does not hold a certificate", cert_path.display());
    }
    let (issuer, serial, public) = cert_fields(&cert)
        .with_context(|| format!("reading the certificate {}", cert_path.display()))?;
    if public != spki(&private)? {
        bail!(
            "the certificate {} is not that of the key {}",
            cert_path.display(),
            key_path.display()
        );
    }
    Ok(Key {
        private,
        cert,
        issuer,
        serial,
    })
}

/// The DER encodings of the issuer, serial number and public key of the
/// certificate `cert`.
fn cert_fields(cert: &[u8]) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let certificate = der::Reader(cert).expect(der::SEQUENCE)?;
    let tbs = der::Reader(certificate.content).expect(der::SEQUENCE)?;
    let mut fields = der::Reader(tbs.content);
    fields.expect(0xa0)?;
    let serial = fields.expect(der::INTEGER)?.raw.to_vec();
    fields.expect(der::SEQUENCE)?;
    let issuer = fields.expect(der::SEQUENCE)?.raw.to_vec();
    fields.expect(der::SEQUENCE)?;
    fields.expect(der::SEQUENCE)?;
    let public = fields.expect(der::SEQUENCE)?.raw.to_vec();
    Ok((issuer, serial, public))
}

fn spki(key: &PrivateKey) -> Result<Vec<u8>> {
    let public = key
        .to_public_key()
        .to_public_key_der()
        .map_err(|e| anyhow::anyhow!("encoding the public key: {e}"))?;
    Ok(public.as_bytes().to_vec())
}

fn create(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let mut rng = getrandom::SysRng;
    let private = PrivateKey::new(&mut rng, BITS)?;
    let mut serial = [0u8; 16];
    getrandom::fill(&mut serial).map_err(|e| anyhow::anyhow!("no random numbers: {e}"))?;
    serial[0] &= 0x7f;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let cert = certificate(&private, &serial, now)?;

    let pem = private
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| anyhow::anyhow!("encoding the key: {e}"))?;
    let key_path = dir.join(KEY_FILE);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_path)
        .and_then(|mut f| f.write_all(pem.as_bytes()))
        .with_context(|| format!("writing {}", key_path.display()))?;
    let pem = pem_rfc7468::encode_string("CERTIFICATE", LineEnding::LF, &cert)
        .map_err(|e| anyhow::anyhow!("encoding the certificate: {e}"))?;
    let cert_path = dir.join(CERT_FILE);
    fs::write(&cert_path, pem).with_context(|| format!("writing {}", cert_path.display()))?;
    Ok(())
}

/// The self-signed X.509 certificate of `key` with the serial number
/// `serial`, valid from `now` for ten years.
fn certificate(key: &PrivateKey, serial: &[u8], now: u64) -> Result<Vec<u8>> {
    let name = der::seq(&[&der::set_of(&[&der::seq(&[
        &der::oid(&[2, 5, 4, 3]),
        &der::utf8(SUBJECT),
    ])])]);
    let public = spki(key)?;
    let bit_string = der::Reader(&public)
        .expect(der::SEQUENCE)
        .and_then(|s| {
            let mut r = der::Reader(s.content);
            r.expect(der::SEQUENCE)?;
            r.expect(0x03)
        })?
        .content;
    // The key identifier is the first 160 bits of the SHA-256 digest of the
    // public key's bits.
    let key_id = &Sha256::digest(&bit_string[1..])[..20];
    let extension = |arcs: &[u64], critical: bool, value: &[u8]| {
        let critical = if critical { der::boolean(true) } else { vec![] };
        der::seq(&[&der::oid(arcs), &critical, &der::octets(value)])
    };
    let extensions = der::context(
        3,
        &[&der::seq(&[
            // basicConstraints: a CA, so that the certificate can also be
            // the platform key and key exchange key.
            &extension(&[2, 5, 29, 19], true, &der::seq(&[&der::boolean(true)])),
            // subjectKeyIdentifier
            &extension(&[2, 5, 29, 14], false, &der::octets(key_id)),
            // authorityKeyIdentifier
            &extension(
                &[2, 5, 29, 35],
                false,
                &der::seq(&[&der::tlv(0x80, key_id)]),
            ),
        ])],
    );
    let tbs = der::seq(&[
        &der::context(0, &[&der::small_uint(2)]),
        &der::uint(serial),
        &algorithm(SHA256_WITH_RSA),
        &name,
        &der::seq(&[&der::time(now, 0), &der::time(now, VALIDITY_YEARS)]),
        &name,
        &public,
        &extensions,
    ]);
    let signature = key.pkcs1v15_sign::<Sha256>(&tbs)?;
    Ok(der::seq(&[
        &tbs,
        &algorithm(SHA256_WITH_RSA),
        &der::bits(&signature),
    ]))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A key directory in the system's temporary directory, removed when
    /// dropped.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(name: &str) -> TempDir {
            let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn creates_a_key_once_and_reuses_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("signing-key");
        let first = load_or_create(&dir.0).unwrap();
        let mode = fs::metadata(dir.0.join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let second = load_or_create(&dir.0).unwrap();
        assert_eq!(first.cert, second.cert);
        assert_eq!(first.serial, second.serial);

        let signature = second.sign(b"data").unwrap();
        second
            .private
            .to_public_key()
            .pkcs1v15_verify::<Sha256>(b"data", &signature)
            .unwrap();
    }

    #[test]
    fn certificate_is_self_signed_for_ten_years() {
        let dir = TempDir::new("signing-cert");
        let key = load_or_create(&dir.0).unwrap();
        let certificate = der::Reader(&key.cert).expect(der::SEQUENCE).unwrap();
        let mut parts = der::Reader(certificate.content);
        let tbs = parts.expect(der::SEQUENCE).unwrap();
        parts.expect(der::SEQUENCE).unwrap();
        let signature = parts.expect(0x03).unwrap();
        key.private
            .to_public_key()
            .pkcs1v15_verify::<Sha256>(tbs.raw, &signature.content[1..])
            .unwrap();

        let mut fields = der::Reader(tbs.content);
        for _ in 0..3 {
            fields.next().unwrap();
        }
        let issuer = fields.next().unwrap().raw;
        let validity = fields.expect(der::SEQUENCE).unwrap();
        let subject = fields.next().unwrap().raw;
        assert_eq!(issuer, subject);
        assert!(subject.ends_with(SUBJECT.as_bytes()));
        let mut times = der::Reader(validity.content);
        let from = times.next().unwrap().content;
        let until = times.next().unwrap().content;
        let year = |t: &[u8]| -> u32 { std::str::from_utf8(&t[..2]).unwrap().parse().unwrap() };
        assert_eq!(year(until), year(from) + 10);
        assert_eq!(&until[2..], &from[2..]);
    }

    #[test]
    fn refuses_a_certificate_of_another_key() {
        let one = TempDir::new("signing-one");
        let two = TempDir::new("signing-two");
        load_or_create(&one.0).unwrap();
        load_or_create(&two.0).unwrap();
        fs::copy(two.0.join(CERT_FILE), one.0.join(CERT_FILE)).unwrap();
        assert!(load(&one.0).is_err());
    }
}
