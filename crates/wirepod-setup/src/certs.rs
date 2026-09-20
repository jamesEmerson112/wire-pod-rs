//! Translation of `pkg/wirepod/setup/certs.go`.
//!
//! Go reads the certificate paths and the API config from globals; here the
//! caller passes them. Go's key size is 1028 bits, which `ring` and every other
//! TLS-oriented signer refuses, so the key is generated and the signature made
//! by the `rsa` crate and handed to rcgen through its [`SigningKey`] seam.

use std::io;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};

use rcgen::{
    BasicConstraints, CertificateParams, CustomExtension, DistinguishedName,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyUsagePurpose, PKCS_RSA_SHA256, PublicKeyData,
    SanType, SerialNumber, SignatureAlgorithm, SigningKey,
};
use rsa::RsaPrivateKey;
use rsa::pkcs1::{EncodeRsaPrivateKey, EncodeRsaPublicKey, LineEnding};
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use wirepod_core::config::ServerConfig;
use wirepod_core::go_marshal;
use wirepod_core::paths::DataDir;
use wirepod_core::persist::write_atomic;

/// The mode of every `os.WriteFile` in this file (`certs.go:83`, `:88`, `:123`).
pub const CERT_FILE_MODE: u32 = 0o777;

/// Go's key size at `certs.go:45` and `:61`.
const KEY_BITS: usize = 1028;

#[derive(Debug, thiserror::Error)]
pub enum CertError {
    #[error("{0}")]
    Key(#[from] rsa::Error),
    #[error("{0}")]
    Certificate(#[from] rcgen::Error),
    #[error("{0}")]
    Pkcs1(#[from] rsa::pkcs1::Error),
    #[error("{0}")]
    Write(#[from] io::Error),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ClientServerConfig {
    pub jdocs: String,
    #[serde(rename = "tms")]
    pub token: String,
    pub chipper: String,
    pub check: String,
    pub logfiles: String,
    pub appkey: String,
}

/// Go's `vars.GetOutboundIP`. `wirepod-server`'s mDNS module carries its own
/// copy of the same three lines.
// TODO(M5): vars.GetOutboundIP()
fn get_outbound_ip() -> IpAddr {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return IpAddr::V4(Ipv4Addr::LOCALHOST);
    };
    if socket.connect("8.8.8.8:80").is_err() {
        return IpAddr::V4(Ipv4Addr::LOCALHOST);
    }
    socket
        .local_addr()
        .map(|addr| addr.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

/// Go's `Time.AddDate(years, 0, 0)`, which normalizes 29 February in a year
/// that is not a leap year into 1 March.
fn add_years(t: OffsetDateTime, years: i32) -> OffsetDateTime {
    let year = t.year() + years;
    match t.replace_year(year) {
        Ok(moved) => moved,
        Err(_) => t
            .replace_day(1)
            .and_then(|t| t.replace_month(time::Month::March))
            .and_then(|t| t.replace_year(year))
            .unwrap_or(t),
    }
}

/// An RSA key of a size rcgen's own backends reject, signing for rcgen.
struct RsaSigningKey {
    key: RsaPrivateKey,
    public_der: Vec<u8>,
}

impl RsaSigningKey {
    fn generate() -> Result<Self, CertError> {
        let key = RsaPrivateKey::new(&mut rand::thread_rng(), KEY_BITS)?;
        let public_der = key.to_public_key().to_pkcs1_der()?.as_bytes().to_vec();
        Ok(Self { key, public_der })
    }
}

impl PublicKeyData for RsaSigningKey {
    fn der_bytes(&self) -> &[u8] {
        &self.public_der
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        &PKCS_RSA_SHA256
    }
}

impl SigningKey for RsaSigningKey {
    fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        self.key
            .sign(rsa::Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(msg))
            .map_err(|_| rcgen::Error::RemoteKeyError)
    }
}

/// Go's `SubjectKeyId`, which rcgen writes no extension for on a leaf.
fn subject_key_id(id: &[u8]) -> CustomExtension {
    CustomExtension::from_oid_content(&[2, 5, 29, 14], yasna_octet_string(id))
}

/// The DER `OCTET STRING` the subjectKeyIdentifier extension wraps.
fn yasna_octet_string(id: &[u8]) -> Vec<u8> {
    let mut der = vec![0x04, u8::try_from(id.len()).unwrap_or(0)];
    der.extend_from_slice(id);
    der
}

/// creates and exports a priv/pub key combo generated with IP address
pub async fn create_cert_combo(data: &DataDir) -> Result<(), CertError> {
    // get preferred IP address of machine
    create_cert_combo_for(data, get_outbound_ip()).await
}

/// [`create_cert_combo`] with the outbound address already resolved, so that a
/// test never has to look one up.
pub async fn create_cert_combo_for(data: &DataDir, ip_addr: IpAddr) -> Result<(), CertError> {
    // ca certificate
    let now = OffsetDateTime::now_utc();
    let mut ca = CertificateParams::default();
    ca.serial_number = Some(SerialNumber::from(2019u64));
    ca.distinguished_name = DistinguishedName::new();
    ca.not_before = now;
    ca.not_after = add_years(now, 30);
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    ca.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
    ];
    let ca_priv_key = RsaSigningKey::generate()?;

    // create actual certificate
    let mut cert = CertificateParams::default();
    cert.serial_number = Some(SerialNumber::from(1658u64));
    cert.distinguished_name = DistinguishedName::new();
    cert.subject_alt_names = vec![SanType::IpAddress(ip_addr)];
    cert.not_before = now;
    cert.not_after = add_years(now, 10);
    cert.custom_extensions = vec![subject_key_id(&[1, 2, 3, 4, 6])];
    cert.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    cert.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    let cert_priv_key = RsaSigningKey::generate()?;
    // Go never emits the CA certificate, only signs with its key and template.
    let cert_bytes = cert.signed_by(&cert_priv_key, &Issuer::new(ca, ca_priv_key))?;
    let cert_pem = cert_bytes.pem();
    let cert_priv_key_pem = cert_priv_key.key.to_pkcs1_pem(LineEnding::LF)?;

    // export certificates
    let _ = std::fs::create_dir_all(data.certs_dir());
    tracing::info!(
        comp = "",
        "Outputting certificate to {}",
        data.cert_path().display()
    );
    write_atomic(data.cert_path(), cert_pem.into_bytes(), CERT_FILE_MODE).await?;
    tracing::info!(
        comp = "",
        "Outputting private key to {}",
        data.key_path().display()
    );
    write_atomic(
        data.key_path(),
        cert_priv_key_pem.as_bytes().to_vec(),
        CERT_FILE_MODE,
    )
    .await?;
    // vars.ChipperCert, vars.ChipperKey and vars.ChipperKeysLoaded have no
    // counterpart here: the listener reads the pair off disk when it boots.

    Ok(())
}

/// The struct Go marshals, separated out so it can be read without writing it.
//{"jdocs": "escapepod.local:443", "tms": "escapepod.local:443", "chipper": "escapepod.local:443", "check": "escapepod.local/ok:80", "logfiles": "s3://anki-device-logs-prod/victor", "appkey": "oDoa0quieSeir6goowai7f"}
pub fn client_server_config(server: &ServerConfig) -> ClientServerConfig {
    let mut config = ClientServerConfig::default();
    if server.epconfig {
        config.jdocs = "escapepod.local:443".to_string();
        config.token = "escapepod.local:443".to_string();
        config.chipper = "escapepod.local:443".to_string();
        config.check = "escapepod.local/ok".to_string();
        config.logfiles = "s3://anki-device-logs-prod/victor".to_string();
        config.appkey = "oDoa0quieSeir6goowai7f".to_string();
    } else {
        let ip = get_outbound_ip();
        let ip_string = ip.to_string();
        let url = ip_string.clone() + ":" + &server.port;
        config.jdocs = url.clone();
        config.token = url.clone();
        config.chipper = url;
        config.check = ip_string + "/ok";
        config.logfiles = "s3://anki-device-logs-prod/victor".to_string();
        config.appkey = "oDoa0quieSeir6goowai7f".to_string();
    }
    config
}

/// outputs a server config to ../certs/server_config.json
pub async fn create_server_config(data: &DataDir, server: &ServerConfig) {
    let _ = std::fs::create_dir_all(data.certs_dir());
    let config = client_server_config(server);
    // Go discards the marshal error and the write error, and so does this.
    let write_bytes = go_marshal(&config).unwrap_or_default();
    let _ = write_atomic(data.server_config_path(), write_bytes, CERT_FILE_MODE).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("wirepod-setup-{name}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("could not create the temporary directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn the_generated_certificate_carries_an_empty_subject_and_the_address_as_its_only_san() {
        let dir = TempDir::new("certs");
        let data = DataDir::rooted(&dir.0);
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        create_cert_combo_for(&data, ip).await.expect("generated");

        let pem = std::fs::read_to_string(data.cert_path()).expect("read the certificate");
        let key = std::fs::read_to_string(data.key_path()).expect("read the key");
        assert!(key.starts_with("-----BEGIN RSA PRIVATE KEY-----"));
        let der = x509_parser::pem::parse_x509_pem(pem.as_bytes())
            .expect("pem")
            .1;
        let cert = der.parse_x509().expect("x509");
        assert_eq!(cert.subject().iter().count(), 0);
        assert_eq!(cert.tbs_certificate.serial, 1658u32.into());
        let san = cert
            .subject_alternative_name()
            .expect("san")
            .expect("present");
        assert_eq!(
            san.value.general_names,
            vec![x509_parser::extensions::GeneralName::IPAddress(&[
                127, 0, 0, 1
            ])]
        );
        let parsed = cert.public_key().parsed().expect("public key");
        let x509_parser::public_key::PublicKey::RSA(rsa) = parsed else {
            panic!("the generated key is not RSA");
        };
        // `x509_parser::public_key::RSAPublicKey::key_size` mistakes the top
        // byte of a 1028-bit modulus for DER padding, so count the bits here.
        let modulus = rsa.modulus.strip_prefix(&[0]).unwrap_or(rsa.modulus);
        let leading = usize::try_from(modulus[0].leading_zeros()).expect("fits");
        assert_eq!(modulus.len() * 8 - leading, KEY_BITS);
    }

    #[test]
    fn the_escape_pod_server_config_holds_the_six_keys_the_robot_reads() {
        let server = ServerConfig {
            epconfig: true,
            port: "443".to_string(),
            extra: Default::default(),
        };
        let bytes = go_marshal(&client_server_config(&server)).expect("marshalled");
        assert_eq!(
            String::from_utf8(bytes).expect("utf8"),
            concat!(
                r#"{"jdocs":"escapepod.local:443","tms":"escapepod.local:443","#,
                r#""chipper":"escapepod.local:443","check":"escapepod.local/ok","#,
                r#""logfiles":"s3://anki-device-logs-prod/victor","#,
                r#""appkey":"oDoa0quieSeir6goowai7f"}"#,
            )
        );
    }
}
