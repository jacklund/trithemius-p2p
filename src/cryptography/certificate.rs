use crate::tor::{connection::read_line, error::TorError};
use base64;
use byteorder::{BigEndian, ReadBytesExt};
use chrono::{DateTime, TimeZone, Utc};
use ed25519_dalek::Verifier;
use ed25519_dalek::{PublicKey, Signature, PUBLIC_KEY_LENGTH};
use std::io::Read;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{FramedRead, LinesCodec};

#[derive(Debug)]
enum CertifiedKey {
    PublicKey(PublicKey),
}

impl std::default::Default for CertifiedKey {
    fn default() -> Self {
        Self::PublicKey(PublicKey::from_bytes(&[0; PUBLIC_KEY_LENGTH]).unwrap())
    }
}

#[derive(Debug, PartialEq)]
pub enum CertificateType {
    IdSigning = 4,
    SigningLink = 5,
    SigningAuth = 6,
    SigningHsDesc = 8,
    AuthHsIpKey = 9,
    OnionId = 10,
    CrossHsIpKeys = 11,
}

impl TryFrom<u8> for CertificateType {
    type Error = TorError;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            x if x == CertificateType::IdSigning as u8 => Ok(CertificateType::IdSigning),
            x if x == CertificateType::SigningLink as u8 => Ok(CertificateType::SigningLink),
            x if x == CertificateType::SigningAuth as u8 => Ok(CertificateType::SigningAuth),
            x if x == CertificateType::SigningHsDesc as u8 => Ok(CertificateType::SigningHsDesc),
            x if x == CertificateType::AuthHsIpKey as u8 => Ok(CertificateType::AuthHsIpKey),
            x if x == CertificateType::OnionId as u8 => Ok(CertificateType::OnionId),
            x if x == CertificateType::CrossHsIpKeys as u8 => Ok(CertificateType::CrossHsIpKeys),
            _ => Err(TorError::ProtocolError(format!(
                "Unknown certificate type: {}",
                v
            ))),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum SignedKeyType {
    ED25519 = 1,
    RSASHA256 = 2,
    X509SHA256 = 3,
}

impl TryFrom<u8> for SignedKeyType {
    type Error = TorError;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            x if x == SignedKeyType::ED25519 as u8 => Ok(SignedKeyType::ED25519),
            x if x == SignedKeyType::RSASHA256 as u8 => Ok(SignedKeyType::RSASHA256),
            x if x == SignedKeyType::X509SHA256 as u8 => Ok(SignedKeyType::X509SHA256),
            _ => Err(TorError::ProtocolError(format!(
                "Unknown signed key type: {}",
                v
            ))),
        }
    }
}

#[derive(Debug)]
enum ExtensionFlags {
    None = 0,
    IncludeSigningKey = 1,
}

impl TryFrom<u8> for ExtensionFlags {
    type Error = TorError;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            x if x == ExtensionFlags::None as u8 => Ok(ExtensionFlags::None),
            x if x == ExtensionFlags::IncludeSigningKey as u8 => {
                Ok(ExtensionFlags::IncludeSigningKey)
            }
            _ => Err(TorError::ProtocolError(format!(
                "Unknown extension flag: {}",
                v
            ))),
        }
    }
}

#[derive(Debug)]
enum Extension {
    SignedWithKey {
        flags: ExtensionFlags,
        public_key: PublicKey,
    },
}

impl Extension {
    fn from_bytes(reader: &mut dyn Read) -> Result<Extension, Box<dyn std::error::Error>> {
        let length: usize = reader.read_u16::<BigEndian>().unwrap().into();
        let extension_type = reader.read_u8().unwrap();
        if extension_type != 4 {
            Err(TorError::ProtocolError(format!(
                "Unknown extension type {}",
                extension_type
            )))?;
        }
        let flags = reader.read_u8().unwrap().try_into()?;
        let mut buf = vec![0; length];
        reader.read_exact(&mut buf)?;

        Ok(Extension::SignedWithKey {
            flags,
            public_key: PublicKey::from_bytes(&buf)?,
        })
    }
}

#[derive(Debug)]
pub struct ED25519Certificate {
    version: u8,
    cert_type: CertificateType,
    expiration_date: DateTime<Utc>,
    key_type: SignedKeyType,
    certified_key: CertifiedKey,
    extensions: Vec<Extension>,
    signature: Signature,
}

impl std::default::Default for ED25519Certificate {
    fn default() -> Self {
        Self {
            version: 0,
            cert_type: CertificateType::IdSigning,
            expiration_date: Utc::now(),
            key_type: SignedKeyType::ED25519,
            certified_key: CertifiedKey::PublicKey(
                PublicKey::from_bytes(&[0; PUBLIC_KEY_LENGTH]).unwrap(),
            ),
            extensions: vec![],
            signature: Signature::from_bytes(&[0; 64]).unwrap(),
        }
    }
}

const ED25519_CERTIFICATE_BEGIN: &str = "-----BEGIN ED25519 CERT-----";
const ED25519_CERTIFICATE_END: &str = "-----END ED25519 CERT-----";

pub async fn read_ed25519_certificate_data<S: AsyncRead + Unpin>(
    stream: S,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut framed = FramedRead::new(stream, LinesCodec::new());
    let mut lines = vec![];
    loop {
        let line = read_line(&mut framed).await?;
        if line == ED25519_CERTIFICATE_BEGIN {
            continue;
        } else if line == ED25519_CERTIFICATE_END {
            break;
        } else {
            lines.push(line);
        }
    }
    Ok(base64::decode(lines.join(""))?)
}

pub async fn parse_ed25519_certificate<S: AsyncRead + Unpin>(
    stream: S,
) -> Result<ED25519Certificate, Box<dyn std::error::Error>> {
    // Read the certificate
    let bytes = read_ed25519_certificate_data(stream).await?;

    // Set aside the bytes used for signature
    let bytes_to_sign = &bytes.clone()[..bytes.len() - 64];

    // Parse the data
    let mut certificate = ED25519Certificate::default();
    let mut cursor = std::io::Cursor::new(bytes);
    certificate.version = cursor.read_u8()?;
    certificate.cert_type = cursor.read_u8()?.try_into()?;

    // Parse the expiration date, which is encoded as a long int as hourse since the epoch
    let hours_since_epoch = cursor.read_u32::<BigEndian>()?;
    certificate.expiration_date = Utc.timestamp((3600 * hours_since_epoch).into(), 0);

    // Cert key type
    certificate.key_type = cursor.read_u8()?.try_into()?;

    // Read the certified key
    let mut buf: [u8; 32] = [0; 32];
    cursor.read_exact(&mut buf)?;
    certificate.certified_key = CertifiedKey::PublicKey(PublicKey::from_bytes(&buf)?);

    // Read the extensions
    let num_extensions = cursor.read_u8()?;
    for _ in 0..num_extensions {
        certificate
            .extensions
            .push(Extension::from_bytes(&mut cursor)?);
    }

    // Read the signature
    let mut buf: [u8; 64] = [0; 64];
    cursor.read_exact(&mut buf)?;
    certificate.signature = buf.into();

    let mut verified = false;
    for extension in &certificate.extensions {
        match extension {
            Extension::SignedWithKey {
                flags: _,
                public_key,
            } => match public_key.verify(&bytes_to_sign, &certificate.signature) {
                Ok(()) => {
                    verified = true;
                }
                Err(error) => Err(error)?,
            },
        }
    }

    match verified {
        true => Ok(certificate),
        false => Err(TorError::protocol_error("Certificate not verified"))?,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_ed25519_certificate() -> Result<(), Box<dyn std::error::Error>> {
        const CERT_DATA: &str = "-----BEGIN ED25519 CERT-----\n\
            AQgABwb/AffeimvM/+cAQYvNL0GE8HplhxLTWfkVGZtgI0k/Q7yeAQAgBABsS3Ux\n\
            NJobxQ6fs1/DqCETaUhG5vwDAoOVq7fol9CGi30lG+HIbtguSf/TN7wEdAPLF8BQ\n\
            WfzZnex9NDI8oBHAT0oQiaUzHcQlfOlrUEfNti3IWSQD3lLEo0CSCM6GgAc=\n\
            -----END ED25519 CERT-----";
        let cursor = std::io::Cursor::new(CERT_DATA);
        let certificate = parse_ed25519_certificate(cursor).await?;
        println!("certificate = {:?}", certificate);

        assert_eq!(1, certificate.version);
        assert_eq!(CertificateType::SigningHsDesc, certificate.cert_type);
        assert_eq!(SignedKeyType::ED25519, certificate.key_type);
        Ok(())
    }
}
