use crate::tor::{auth::TorAuthentication, error::TorError};
use futures::{SinkExt, StreamExt};
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;
use std::marker::Send;
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, oneshot};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

pub struct OnionService {
    virt_port: u16,
    target_port: u16,
    service_id: String,
}

#[derive(Debug, PartialEq)]
enum ControlResponseType {
    SyncResponse,
    AsyncResponse(String),
}

#[derive(Debug)]
pub struct ControlResponse {
    code: u16,
    indicator: char,
    response_type: ControlResponseType,
    response: Vec<String>,
}

impl<'a> From<regex::Captures<'a>> for ControlResponse {
    fn from(captures: regex::Captures) -> Self {
        let code = captures["code"].parse::<u16>().unwrap();
        let indicator = captures["type"].chars().last().unwrap();
        let response_type = if code >= 600 {
            ControlResponseType::AsyncResponse(captures["keyword"].to_string())
        } else {
            ControlResponseType::SyncResponse
        };
        let response = vec![captures["response"].to_string()];

        Self {
            code,
            indicator,
            response_type,
            response,
        }
    }
}

async fn parse_control_response<S: StreamExt<Item = Result<String, LinesCodecError>> + Unpin>(
    reader: &mut S,
) -> Result<ControlResponse, TorError> {
    lazy_static! {
        static ref AsyncRegex: Regex = Regex::new(
            r"^(?P<code>6\d{2})(?P<type>[\- +])(?P<keyword>[^ ]*) (?P<response>.*)\r?\n?$"
        )
        .unwrap();
        static ref SyncRegex: Regex =
            Regex::new(r"^(?P<code>\d{3})(?P<type>[\- +])(?P<response>.*)\r?\n?$").unwrap();
    }

    let line = read_line(reader).await?;
    match AsyncRegex.captures(&line) {
        Some(captures) => Ok(captures.into()),
        None => match SyncRegex.captures(&line) {
            Some(captures) => Ok(captures.into()),
            None => Err(TorError::ProtocolError(format!(
                "Unknown response: {}",
                line
            ))),
        },
    }
}

async fn read_control_response<S: StreamExt<Item = Result<String, LinesCodecError>> + Unpin>(
    reader: &mut S,
) -> Result<ControlResponse, TorError> {
    let mut control_response = parse_control_response(reader).await?;
    match control_response.indicator {
        ' ' => Ok(control_response),
        '-' => {
            let mut aggregated_response = vec![control_response.response[0].clone()];
            loop {
                let control_response = parse_control_response(reader).await?;
                match control_response.indicator {
                    ' ' => match control_response.code {
                        250 => break,
                        _ => Err(TorError::ProtocolError(format!(
                            "Unexpected code {} during mid response",
                            control_response.code,
                        )))?,
                    },
                    '-' => aggregated_response.extend(control_response.response),
                    _ => Err(TorError::protocol_error(
                        "Unexpected response type during mid response",
                    ))?,
                }
            }
            control_response.response = aggregated_response;
            Ok(control_response)
        }
        '+' => {
            let mut response = vec![control_response.response[0].clone()];
            loop {
                let line = read_line(reader).await?;
                if line == "." {
                    break;
                }
                response.push(line);
            }
            control_response.response = response;
            Ok(control_response)
        }
        _ => Err(TorError::ProtocolError(format!(
            "Unexpected response type '{}': {}{}{}",
            control_response.indicator,
            control_response.code,
            control_response.indicator,
            control_response.response.join(" "),
        ))),
    }
}

pub(crate) async fn read_line<S: StreamExt<Item = Result<String, LinesCodecError>> + Unpin>(
    reader: &mut S,
) -> Result<String, TorError> {
    match reader.next().await {
        Some(Ok(line)) => Ok(line),
        Some(Err(error)) => Err(error.into()),
        None => Err(TorError::protocol_error("Unexpected EOF on stream")),
    }
}

pub struct TorControlConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Sync + Send + 'static,
{
    writer: FramedWrite<WriteHalf<S>, LinesCodec>,
    registration_sender: mpsc::UnboundedSender<(String, oneshot::Sender<ControlResponse>)>,
    sync_receiver: mpsc::UnboundedReceiver<ControlResponse>,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Sync + Send + 'static> TorControlConnection<S> {
    fn connect(stream: S) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        let (registration_sender, registration_receiver) = mpsc::unbounded_channel();
        let (sync_sender, sync_receiver) = mpsc::unbounded_channel();
        Self::start_read_loop(reader, registration_receiver, sync_sender);
        Self {
            writer: FramedWrite::new(writer, LinesCodec::new()),
            registration_sender,
            sync_receiver,
        }
    }

    fn start_read_loop(
        reader: ReadHalf<S>,
        mut registration_receiver: mpsc::UnboundedReceiver<(
            String,
            oneshot::Sender<ControlResponse>,
        )>,
        sync_sender: mpsc::UnboundedSender<ControlResponse>,
    ) {
        tokio::spawn(async move {
            let mut async_map = HashMap::<String, oneshot::Sender<ControlResponse>>::new();
            let mut reader = FramedRead::new(reader, LinesCodec::new());
            loop {
                tokio::select! {
                    control_response = read_control_response(&mut reader) => {
                        let control_response = control_response.unwrap();
                        match control_response.response_type {
                            ControlResponseType::AsyncResponse(ref keyword) => {
                                match async_map.remove(keyword) {
                                    Some(sender) => {
                                        sender.send(control_response).unwrap();
                                    }
                                    None => {
                                        panic!("Got response without a registered callback!");
                                    }
                                }
                            },
                            ControlResponseType::SyncResponse => {
                                sync_sender.send(control_response).unwrap();
                            },
                        }
                    },
                    Some((keyword, sender)) = registration_receiver.recv() => {
                        async_map.insert(keyword, sender);
                    }
                };
            }
        });
    }

    async fn write(&mut self, data: &str) -> Result<(), TorError> {
        self.writer.send(data).await?;
        Ok(())
    }

    async fn authenticate(&mut self, method: TorAuthentication) -> Result<(), TorError> {
        method.authenticate(self).await?;
        Ok(())
    }

    async fn read_sync_response(&mut self) -> Result<ControlResponse, TorError> {
        match self.sync_receiver.recv().await {
            Some(control_response) => Ok(control_response),
            None => Err(TorError::protocol_error(
                "Got unexpected EOF on sync_receiver",
            )),
        }
    }

    async fn register_for_async_response(
        &mut self,
        keywords: &[&str],
    ) -> Result<Vec<oneshot::Receiver<ControlResponse>>, TorError> {
        self.write(&format!("SETEVENTS {}", keywords.join(" ")))
            .await?;
        let mut receivers = vec![];
        for keyword in keywords {
            let (sender, receiver) = oneshot::channel();
            self.registration_sender
                .send((keyword.to_string(), sender))
                .unwrap();
            receivers.push(receiver);
        }
        Ok(receivers)
    }

    pub async fn send_sync_command(
        &mut self,
        command: &str,
        arguments: Option<String>,
    ) -> Result<ControlResponse, TorError> {
        match arguments {
            None => self.write(&format!("{}", command)).await?,
            Some(arguments) => self.write(&format!("{} {}", command, arguments)).await?,
        };
        match self.read_sync_response().await {
            Ok(control_response) => match control_response.code {
                250 | 251 => Ok(control_response),
                _ => Err(TorError::ProtocolError(control_response.response.join(" "))),
            },
            Err(error) => Err(error),
        }
    }

    async fn send_async_command(
        &mut self,
        command: &str,
        keywords: &[&str],
        arguments: Option<String>,
    ) -> Result<Vec<ControlResponse>, TorError> {
        let receivers = self.register_for_async_response(keywords).await?;
        let mut responses = vec![];
        for receiver in receivers {
            responses.push(receiver.await?);
        }

        Ok(responses)
    }

    async fn create_transient_onion_service(
        &mut self,
        virt_port: u16,
        target_port: u16,
    ) -> Result<OnionService, TorError> {
        let control_response = self
            .send_sync_command(
                "ADD_ONION",
                Some(format!(
                    "NEW:BEST Flags=DiscardPK Port={},{}",
                    virt_port, target_port
                )),
            )
            .await?;
        lazy_static! {
            static ref RE: Regex = Regex::new(r"^[^=]*=(?P<value>.*)$").unwrap();
        }
        match RE.captures(&control_response.response[0]) {
            Some(captures) => Ok(OnionService {
                virt_port,
                target_port,
                service_id: captures["value"].into(),
            }),
            None => Err(TorError::ProtocolError(format!(
                "Unexpected response: {} {}",
                control_response.code,
                control_response.response.join(" "),
            ))),
        }
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
        let control_response = result.unwrap();
        assert_eq!(250, control_response.code);
        assert_eq!(1, control_response.response.len());
        assert_eq!("OK", control_response.response[0]);

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
        let control_response = result.unwrap();
        assert_eq!(250, control_response.code);
        assert_eq!(2, control_response.response.len());
        assert_eq!(
            "ServiceID=647qjf6w3evdbdpy7oidf5vda6rsjzsl5a6ofsaou2v77hj7dmn2spqd",
            control_response.response[0]
        );
        assert_eq!("PrivateKey=ED25519-V3:yLSDc8b11PaIHTtNtvi9lNW99IME2mdrO4k381zDkHv//WRUGrkBALBQ9MbHy2SLA/NmfS7YxmcR/FY8ppRfIA==", control_response.response[1]);

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
        let control_response = result.unwrap();
        assert_eq!(250, control_response.code);
        assert_eq!(3, control_response.response.len());
        assert_eq!("onions/current=", control_response.response[0]);
        assert_eq!(
            "647qjf6w3evdbdpy7oidf5vda6rsjzsl5a6ofsaou2v77hj7dmn2spqd",
            control_response.response[1]
        );
        assert_eq!(
            "yxq7fa63tthq3nd2ul52jjcdpblyai6k3cfmdkyw23ljsoob66z3ywid",
            control_response.response[2]
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_authenticate() -> Result<(), Box<dyn std::error::Error>> {
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

    #[tokio::test]
    async fn test_async_response() -> Result<(), Box<dyn std::error::Error>> {
        let (client, server) = create_mock().await?;
        let mut server = Framed::new(server, LinesCodec::new());
        let mut tor = TorControlConnection::connect(client);
        let mut receivers = tor.register_for_async_response(&["FOO"]).await?;
        let join_handle = tokio::spawn(async move {
            std::thread::sleep(std::time::Duration::from_millis(100));
            server.send("615 FOO Something").await.unwrap();
        });
        let control_response = receivers.remove(0).await?;
        join_handle.await?;
        assert_eq!(615, control_response.code);
        assert_eq!(
            ControlResponseType::AsyncResponse("FOO".to_string()),
            control_response.response_type
        );
        assert_eq!("Something", &control_response.response[0]);

        Ok(())
    }
}
