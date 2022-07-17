use futures::{SinkExt, StreamExt};
use lazy_static::lazy_static;
use regex::Regex;
use std::marker::Send;
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

pub struct OnionService {
    virt_port: u16,
    target_port: u16,
    service_id: String,
}

#[derive(Debug, PartialEq)]
enum TorError {
    AuthenticationError(String),
    ProtocolError(String),
    IOError(String),
}

impl TorError {
    fn authentication_error(msg: &str) -> TorError {
        TorError::AuthenticationError(msg.to_string())
    }

    fn protocol_error(msg: &str) -> TorError {
        TorError::ProtocolError(msg.to_string())
    }
}

impl From<std::io::Error> for TorError {
    fn from(error: std::io::Error) -> TorError {
        TorError::IOError(error.to_string())
    }
}

impl From<LinesCodecError> for TorError {
    fn from(error: LinesCodecError) -> TorError {
        match error {
            LinesCodecError::MaxLineLengthExceeded => TorError::ProtocolError(error.to_string()),
            LinesCodecError::Io(error) => error.into(),
        }
    }
}

pub enum TorAuthentication {
    Null,
    SafeCookie(String),     // Cookie String
    HashedPassword(String), // Password
}

impl TorAuthentication {
    async fn authenticate<S: AsyncRead + AsyncWrite + Unpin + Sync + Send + 'static>(
        &self,
        connection: &mut TorControlConnection<S>,
    ) -> Result<(), TorError> {
        match self {
            TorAuthentication::Null => {
                connection.write("AUTHENTICATE").await?;
                let mut reader = connection.reader.take().unwrap();
                match read_control_response(&mut reader).await {
                    Ok((code, response)) => match code {
                        250 => Ok(()),
                        _ => Err(TorError::AuthenticationError(response.join(" "))),
                    },
                    Err(error) => {
                        connection.reader = Some(reader);
                        Err(error)
                    }
                }
            }
            _ => Err(TorError::authentication_error(
                "Haven't implemented that authentication method yet",
            )),
        }
    }
}

pub struct TorControlConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Sync + Send + 'static,
{
    reader: Option<FramedRead<ReadHalf<S>, LinesCodec>>,
    writer: FramedWrite<WriteHalf<S>, LinesCodec>,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Sync + Send + 'static> TorControlConnection<S> {
    fn connect(stream: S) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self {
            reader: Some(FramedRead::new(reader, LinesCodec::new())),
            writer: FramedWrite::new(writer, LinesCodec::new()),
        }
    }

    async fn write(&mut self, data: &str) -> Result<(), TorError> {
        self.writer.send(data).await?;
        Ok(())
    }

    async fn authenticate(&mut self, method: TorAuthentication) -> Result<(), TorError> {
        method.authenticate(self).await
    }

    async fn start_read_loop(&mut self) {
        let mut reader = self.reader.take().unwrap();
        tokio::spawn(async move {
            loop {
                let (code, response_lines) = read_control_response(&mut reader).await.unwrap();
            }
        });
    }

    // async fn create_transient_onion_service(
    //     &mut self,
    //     virt_port: u16,
    //     target_port: u16,
    // ) -> Result<OnionService, TorError> {
    //     lazy_static! {
    //         static ref RE: Regex = Regex::new(r"^[^=]*=(?P<value>.*)$").unwrap();
    //     }
    //     self.write(&format!(
    //         "ADD_ONION NEW:BEST Flags=DiscardPK Port={},{}",
    //         virt_port, target_port
    //     ))
    //     .await?;
    //     let (code, response) = read_control_response(&mut self.reader.unwrap()).await?;
    //     match code {
    //         250 => match RE.captures(&response[0]) {
    //             Some(captures) => Ok(OnionService {
    //                 virt_port,
    //                 target_port,
    //                 service_id: captures["value"].into(),
    //             }),
    //             None => Err(TorError::ProtocolError(format!(
    //                 "Unexpected response: {} {}",
    //                 code,
    //                 response.join(" "),
    //             ))),
    //         },
    //         _ => Err(TorError::ProtocolError(format!(
    //             "Unexpected response: {} {}",
    //             code,
    //             response.join(" "),
    //         ))),
    //     }
    // }
}

async fn read_line<S: StreamExt<Item = Result<String, LinesCodecError>> + Unpin>(
    reader: &mut S,
) -> Result<String, TorError> {
    match reader.next().await {
        Some(Ok(line)) => Ok(line),
        Some(Err(error)) => Err(error.into()),
        None => Err(TorError::protocol_error("Unexpected EOF on stream")),
    }
}

async fn parse_control_response<S: StreamExt<Item = Result<String, LinesCodecError>> + Unpin>(
    reader: &mut S,
) -> Result<(u16, char, String), TorError> {
    lazy_static! {
        static ref RE: Regex =
            Regex::new(r"^(?P<code>\d{3})(?P<type>[\- +])(?P<response>.*)\r?\n?$").unwrap();
    }

    let line = read_line(reader).await?;
    match RE.captures(&line) {
        Some(captures) => {
            let code = captures["code"].parse::<u16>().unwrap();
            Ok((
                code,
                captures["type"].chars().last().unwrap(),
                captures["response"].to_string(),
            ))
        }
        None => Err(TorError::ProtocolError(format!(
            "Unknown response: {}",
            line
        ))),
    }
}

async fn read_control_response<S: StreamExt<Item = Result<String, LinesCodecError>> + Unpin>(
    reader: &mut S,
) -> Result<(u16, Vec<String>), TorError> {
    let (code, response_type, response) = parse_control_response(reader).await?;
    match response_type {
        ' ' => Ok((code, vec![response.into()])),
        '-' => {
            let mut aggregated_response = vec![response.into()];
            loop {
                let (code, response_type, response) = parse_control_response(reader).await?;
                match response_type {
                    ' ' => match code {
                        250 => break,
                        _ => Err(TorError::ProtocolError(format!(
                            "Unexpected code {} during mid response",
                            code,
                        )))?,
                    },
                    '-' => aggregated_response.push(response),
                    _ => Err(TorError::protocol_error(
                        "Unexpected response type during mid response",
                    ))?,
                }
            }
            Ok((code, aggregated_response))
        }
        '+' => {
            let mut response = vec![response.into()];
            loop {
                let line = read_line(reader).await?;
                if line == "." {
                    break;
                }
                response.push(line);
            }
            Ok((code, response))
        }
        _ => Err(TorError::ProtocolError(format!(
            "Unexpected response type '{}': {}{}{}",
            response_type, code, response_type, response,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::SinkExt;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::codec::{Framed, LinesCodec};

    async fn create_mock() -> Result<(TcpStream, TcpStream), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let join_handle = tokio::spawn(async move { listener.accept().await.unwrap() });
        let client = TcpStream::connect(addr).await?;
        let (server_stream, _) = join_handle.await?;

        Ok((client, server_stream))
    }

    async fn create_framed_mock() -> Result<
        (Framed<TcpStream, LinesCodec>, Framed<TcpStream, LinesCodec>),
        Box<dyn std::error::Error>,
    > {
        let (client, server) = create_mock().await?;
        let reader = Framed::new(client, LinesCodec::new());
        let server = Framed::new(server, LinesCodec::new());

        Ok((reader, server))
    }

    #[tokio::test]
    async fn test_read_good_control_response() -> Result<(), Box<dyn std::error::Error>> {
        // 250 OK response
        let (mut client, mut server) = create_framed_mock().await?;
        server.send("250 OK").await?;
        let result = read_control_response(&mut client).await;
        assert!(result.is_ok());
        let (code, responses) = result.unwrap();
        assert_eq!(250, code);
        assert_eq!(1, responses.len());
        assert_eq!("OK", responses[0]);

        Ok(())
    }

    #[tokio::test]
    async fn test_read_garbled_control_response() -> Result<(), Box<dyn std::error::Error>> {
        // garbled response
        let (mut client, mut server) = create_framed_mock().await?;
        server.send("idon'tknowwhatthisis").await?;
        let result = read_control_response(&mut client).await;
        assert!(result.is_err());
        match result.err() {
            Some(TorError::ProtocolError(_)) => assert!(true),
            _ => assert!(false),
        }

        // Multiline response
        let (mut client, mut server) = create_framed_mock().await?;
        server
            .send("250-ServiceID=647qjf6w3evdbdpy7oidf5vda6rsjzsl5a6ofsaou2v77hj7dmn2spqd")
            .await?;
        server.send("250-PrivateKey=ED25519-V3:yLSDc8b11PaIHTtNtvi9lNW99IME2mdrO4k381zDkHv//WRUGrkBALBQ9MbHy2SLA/NmfS7YxmcR/FY8ppRfIA==").await?;
        server.send("250 OK").await?;
        let result = read_control_response(&mut client).await;
        assert!(result.is_ok());
        let (code, responses) = result.unwrap();
        assert_eq!(250, code);
        assert_eq!(2, responses.len());
        assert_eq!(
            "ServiceID=647qjf6w3evdbdpy7oidf5vda6rsjzsl5a6ofsaou2v77hj7dmn2spqd",
            responses[0]
        );
        assert_eq!("PrivateKey=ED25519-V3:yLSDc8b11PaIHTtNtvi9lNW99IME2mdrO4k381zDkHv//WRUGrkBALBQ9MbHy2SLA/NmfS7YxmcR/FY8ppRfIA==", responses[1]);

        Ok(())
    }

    #[tokio::test]
    async fn test_read_data_control_response() -> Result<(), Box<dyn std::error::Error>> {
        // Data response
        let (mut client, mut server) = create_framed_mock().await?;
        server.send("250+onions/current=").await?;
        server
            .send("647qjf6w3evdbdpy7oidf5vda6rsjzsl5a6ofsaou2v77hj7dmn2spqd")
            .await?;
        server
            .send("yxq7fa63tthq3nd2ul52jjcdpblyai6k3cfmdkyw23ljsoob66z3ywid")
            .await?;
        server.send(".").await?;
        let result = read_control_response(&mut client).await;
        assert!(result.is_ok());
        let (code, responses) = result.unwrap();
        assert_eq!(250, code);
        assert_eq!(3, responses.len());
        assert_eq!("onions/current=", responses[0]);
        assert_eq!(
            "647qjf6w3evdbdpy7oidf5vda6rsjzsl5a6ofsaou2v77hj7dmn2spqd",
            responses[1]
        );
        assert_eq!(
            "yxq7fa63tthq3nd2ul52jjcdpblyai6k3cfmdkyw23ljsoob66z3ywid",
            responses[2]
        );

        Ok(())
    }

    #[tokio::test]
    async fn authenticate() -> Result<(), Box<dyn std::error::Error>> {
        let (client, server) = create_mock().await?;
        let mut server = Framed::new(server, LinesCodec::new());
        server.send("250 OK").await?;
        let mut tor = TorControlConnection::connect(client);
        let result = tor.authenticate(TorAuthentication::Null).await;
        assert!(result.is_ok());

        let (client, server) = create_mock().await?;
        let mut server = Framed::new(server, LinesCodec::new());
        server.send("551 Oops").await?;
        let mut tor = TorControlConnection::connect(client);
        let result = tor.authenticate(TorAuthentication::Null).await;
        assert!(result.is_err());
        assert_eq!(
            TorError::AuthenticationError("Oops".into()),
            result.unwrap_err()
        );

        Ok(())
    }
}
