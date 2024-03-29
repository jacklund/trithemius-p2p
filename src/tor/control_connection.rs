use crate::tor::{auth::TorAuthentication, error::TorError};
use crate::util;
use base32;
use futures::{SinkExt, StreamExt};
use lazy_static::lazy_static;
use libp2p::{
    multiaddr::{Onion3Addr, Protocol},
    Multiaddr,
};
use log::debug;
use regex::Regex;
use std::borrow::Cow;
use std::net::SocketAddr;
use std::str::FromStr;
use tokio::{
    io::{ReadHalf, WriteHalf},
    net::{TcpStream, ToSocketAddrs},
};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

#[derive(Clone, Debug)]
pub struct OnionService {
    pub virt_port: u16,
    pub listen_address: Multiaddr,
    pub service_id: String,
    pub address: Multiaddr,
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
        static ref ASYNC_REGEX: Regex = Regex::new(
            r"^(?P<code>6\d{2})(?P<type>[\- +])(?P<keyword>[^ ]*) (?P<response>.*)\r?\n?$"
        )
        .unwrap();
        static ref SYNC_REGEX: Regex =
            Regex::new(r"^(?P<code>\d{3})(?P<type>[\- +])(?P<response>.*)\r?\n?$").unwrap();
    }

    let line = read_line(reader).await?;
    match ASYNC_REGEX.captures(&line) {
        Some(captures) => Ok(captures.into()),
        None => match SYNC_REGEX.captures(&line) {
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

pub struct TorControlConnection {
    reader: FramedRead<ReadHalf<TcpStream>, LinesCodec>,
    writer: FramedWrite<WriteHalf<TcpStream>, LinesCodec>,
}

impl TorControlConnection {
    pub async fn connect<A: ToSocketAddrs>(addrs: A) -> Result<Self, TorError> {
        Self::with_stream(TcpStream::connect(addrs).await?)
    }

    pub fn with_stream(stream: TcpStream) -> Result<Self, TorError> {
        let (reader, writer) = tokio::io::split(stream);
        Ok(Self {
            reader: FramedRead::new(reader, LinesCodec::new()),
            writer: FramedWrite::new(writer, LinesCodec::new()),
        })
    }

    async fn write(&mut self, data: &str) -> Result<(), TorError> {
        self.writer.send(data).await?;
        Ok(())
    }

    pub async fn authenticate(&mut self, method: TorAuthentication) -> Result<(), TorError> {
        method.authenticate(self).await?;
        Ok(())
    }

    async fn read_response(&mut self) -> Result<ControlResponse, TorError> {
        read_control_response(&mut self.reader).await
    }

    pub async fn send_command(
        &mut self,
        command: &str,
        arguments: Option<String>,
    ) -> Result<ControlResponse, TorError> {
        match arguments {
            None => self.write(command).await?,
            Some(arguments) => self.write(&format!("{} {}", command, arguments)).await?,
        };
        match self.read_response().await {
            Ok(control_response) => match control_response.code {
                250 | 251 => Ok(control_response),
                _ => Err(TorError::ProtocolError(control_response.response.join(" "))),
            },
            Err(error) => Err(error),
        }
    }

    fn get_listen_address_string(listen_address: &Multiaddr) -> Result<String, TorError> {
        let mut ip_string: Option<String> = None;
        let mut port: Option<u16> = None;
        match util::multiaddr_get(listen_address, |p| {
            matches!(p, Protocol::Ip4(_) | Protocol::Ip6(_))
        }) {
            Some(Protocol::Ip4(ip4)) => {
                ip_string = Some(format!("{}", ip4));
            }
            Some(Protocol::Ip6(ip6)) => {
                ip_string = Some(format!("{}", ip6));
            }
            _ => (),
        }
        if let Some(Protocol::Tcp(tcp_port)) =
            util::multiaddr_get(listen_address, |p| matches!(p, Protocol::Tcp(_)))
        {
            port = Some(tcp_port);
        }
        if ip_string.is_none() || port.is_none() {
            Err(TorError::ProtocolError(format!(
                "Bad address: {}",
                listen_address
            )))
        } else {
            Ok(format!("{}:{}", ip_string.unwrap(), port.unwrap()))
        }
    }

    pub async fn create_transient_onion_service(
        &mut self,
        virt_port: u16,
        listen_address: Multiaddr,
    ) -> Result<OnionService, TorError> {
        // Parse listen address into string
        let listen_address_str = Self::get_listen_address_string(&listen_address)?;

        // Send command to Tor controller
        let control_response = self
            .send_command(
                "ADD_ONION",
                Some(format!(
                    "NEW:BEST Flags=DiscardPK Port={},{}",
                    virt_port, listen_address_str
                )),
            )
            .await?;
        debug!(
            "Sent ADD_ONION command, got control response {:?}",
            control_response
        );

        // Parse the controller response
        lazy_static! {
            static ref RE: Regex = Regex::new(r"^[^=]*=(?P<value>.*)$").unwrap();
        }
        match RE.captures(&control_response.response[0]) {
            Some(captures) => {
                // Parse the Hash value
                let hash_string = &captures["value"];
                let hash: [u8; 35] =
                    base32::decode(base32::Alphabet::RFC4648 { padding: false }, hash_string)
                        .unwrap()
                        .as_slice()
                        .try_into()
                        .unwrap();

                // Create the Multiaddr
                let mut addr = Multiaddr::empty();
                addr.push(Protocol::Onion3(Onion3Addr::from((hash, virt_port))));

                // Return the Onion Service
                Ok(OnionService {
                    virt_port,
                    listen_address: listen_address.clone(),
                    service_id: hash_string.to_string(),
                    address: addr,
                })
            }
            None => Err(TorError::ProtocolError(format!(
                "Unexpected response: {} {}",
                control_response.code,
                control_response.response.join(" "),
            ))),
        }
    }

    // TODO: Do we need this?
    pub async fn get_hidden_service_port_mappings(
        &mut self,
    ) -> Result<Vec<(u16, Multiaddr)>, TorError> {
        let control_response = self
            .send_command("GETCONF", Some("HiddenServicePort".to_string()))
            .await?;
        lazy_static! {
            static ref RE: Regex =
                Regex::new(r"^HiddenServicePort=(?P<virtual_port>\d*) (?P<target_addr>.*)$")
                    .unwrap();
        }
        let mut ret = Vec::new();
        for response in control_response.response.clone() {
            match RE.captures(&response) {
                Some(captures) => {
                    let target_addr: Multiaddr = if captures["target_addr"].starts_with("unix:") {
                        let mut addr = Multiaddr::empty();
                        addr.push(Protocol::Unix(Cow::Borrowed(&captures["target_addr"][5..])));
                        addr
                    } else {
                        let socket_addr = SocketAddr::from_str(&captures["target_addr"]).unwrap();
                        let mut addr = Multiaddr::empty();
                        addr.push(socket_addr.ip().into());
                        addr.push(Protocol::Tcp(socket_addr.port()));
                        addr
                    };
                    ret.push((captures["virtual_port"].parse().unwrap(), target_addr));
                }
                None => Err(TorError::ProtocolError(format!(
                    "Unexpected response: {} {}",
                    control_response.code,
                    control_response.response.join(" "),
                )))?,
            }
        }
        Ok(ret)
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
        let mut tor = TorControlConnection::with_stream(client)?;
        let result = tor.authenticate(TorAuthentication::Null).await;
        assert!(result.is_ok());

        let (client, server) = create_mock().await?;
        let mut server = Framed::new(server, LinesCodec::new());
        server.send("551 Oops").await?;
        let mut tor = TorControlConnection::with_stream(client)?;
        let result = tor.authenticate(TorAuthentication::Null).await;
        assert!(result.is_err());
        // TODO: Fix this!!!
        // assert_eq!(
        //     TorError::AuthenticationError("Oops".into()),
        //     result.unwrap_err()
        // );

        Ok(())
    }
}
