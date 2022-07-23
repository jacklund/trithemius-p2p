use crate::cryptography::certificate::{
    parse_ed25519_certificate, CertifiedKey, ED25519Certificate, Extension,
};
use crate::tor::connection::read_line;
use crate::tor::error::TorError;
use ed25519_dalek::Verifier;
use ed25519_dalek::{PublicKey, Signature, PUBLIC_KEY_LENGTH};
use futures::{Stream, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};
use tokio_util::codec::{Framed, FramedWrite, LinesCodec, LinesCodecError};

const ENCRYPTED_SALT_LENGTH: usize = 16;
const DIGEST_256_LEN: usize = 32;

#[derive(Debug)]
pub struct OnionServiceDescriptor {
    version: u8,
    lifetime: u16,
    certificate: ED25519Certificate,
    revision_counter: u64,
    signing_public_key: PublicKey,
    blinded_public_key: PublicKey,
    superencrypted: Vec<u8>,
    signature: Signature,
}

impl std::default::Default for OnionServiceDescriptor {
    fn default() -> Self {
        Self {
            version: 0,
            lifetime: 0,
            certificate: ED25519Certificate::default(),
            revision_counter: 0,
            signing_public_key: PublicKey::from_bytes(&[0; PUBLIC_KEY_LENGTH]).unwrap(),
            blinded_public_key: PublicKey::from_bytes(&[0; PUBLIC_KEY_LENGTH]).unwrap(),
            superencrypted: vec![],
            signature: Signature::from_bytes(&[0; 64]).unwrap(),
        }
    }
}

pub async fn read_encrypted_message<S: Stream<Item = Result<String, LinesCodecError>> + Unpin>(
    stream: &mut S,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut lines = vec![];
    loop {
        let line = read_line(stream).await?;
        if line == "-----BEGIN MESSAGE-----" {
            continue;
        } else if line == "-----END MESSAGE-----" {
            break;
        } else {
            lines.push(line);
        }
    }
    Ok(base64::decode(lines.join(""))?)
}

pub async fn read_onion_service_descriptor<
    S: AsyncRead + AsyncWrite + Unpin + Sync + Send + 'static,
>(
    stream: &mut S,
) -> Result<OnionServiceDescriptor, Box<dyn std::error::Error>> {
    let mut descriptor = OnionServiceDescriptor::default();
    let mut framed = Framed::new(stream, LinesCodec::new());
    let mut lines = vec![];
    loop {
        let line = read_line(&mut framed).await?;
        match line.split_once(" ") {
            Some((key, value)) => match key {
                "hs-descriptor" => descriptor.version = value.parse()?,
                "descriptor-lifetime" => descriptor.lifetime = value.parse()?,
                "descriptor-signing-cert" => {
                    descriptor.certificate = parse_ed25519_certificate(&mut framed).await?;
                    descriptor.signing_public_key = match descriptor.certificate.certified_key {
                        CertifiedKey::PublicKey(public_key) => public_key,
                    };
                    for extension in &descriptor.certificate.extensions {
                        match extension {
                            Extension::SignedWithKey {
                                flags: _,
                                public_key,
                            } => {
                                descriptor.blinded_public_key = *public_key;
                                break;
                            }
                        }
                    }
                }
                "revision-counter" => descriptor.revision_counter = value.parse()?,
                "superencrypted" => {
                    let superencrypted = read_encrypted_message(&mut framed).await?;
                    if superencrypted.len() <= ENCRYPTED_SALT_LENGTH + DIGEST_256_LEN {
                        Err(TorError::ProtocolError(format!(
                            "superencrypted len of {} is too small",
                            superencrypted.len()
                        )))?;
                    }
                    descriptor.superencrypted = superencrypted;
                }
                "signature" => {
                    let string = lines.join("");
                    let bytes_to_sign = string.as_bytes();
                    let signature_vec = base64::decode(value)?;
                    let signature_len = signature_vec.len();
                    let signature_bytes: [u8; 64] = match signature_vec.try_into() {
                        Ok(bytes) => bytes,
                        Err(_) => Err(TorError::ProtocolError(format!(
                            "Expected 64 bytes, got {} for signature",
                            signature_len,
                        )))?,
                    };
                    let signature = Signature::from_bytes(&signature_bytes)?;
                    if let Err(error) = descriptor
                        .signing_public_key
                        .verify(&bytes_to_sign, &signature)
                    {
                        Err(error)?;
                    }
                    descriptor.signature = signature;
                    break;
                }
                _ => (),
            },
            None => Err(TorError::protocol_error("Unexpected EOF"))?,
        }
        lines.push(line);
    }
    Ok(descriptor)
}
