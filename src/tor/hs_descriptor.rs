use crate::tor::connection::read_line;
use crate::tor::error::TorError;
use futures::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};
use tokio_util::codec::{Framed, FramedWrite, LinesCodec, LinesCodecError};

#[derive(Debug, Default)]
pub struct OnionDescriptor {
    version: u8,
    lifetime: u16,
}

pub async fn read_onion_descriptor<S: AsyncRead + AsyncWrite + Unpin + Sync + Send + 'static>(
    stream: S,
) -> Result<OnionDescriptor, Box<dyn std::error::Error>> {
    let mut descriptor = OnionDescriptor::default();
    let mut framed = Framed::new(stream, LinesCodec::new());
    loop {
        match read_line(&mut framed).await?.split_once(" ") {
            Some((key, value)) => match key {
                "hs-descriptor" => descriptor.version = value.parse()?,
                "descriptor-lifetime" => descriptor.lifetime = value.parse()?,
                _ => (),
            },
            None => Err(TorError::protocol_error("Unexpected EOF"))?,
        }
    }
    unimplemented!()
}
