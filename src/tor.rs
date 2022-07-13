use async_recursion::async_recursion;
use lazy_static::lazy_static;
use regex::Regex;
use std::net::SocketAddr;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio_test::io::Mock;

pub struct OnionService {
    virt_port: u16,
    target_port: u16,
    service_id: String,
}

#[derive(Debug)]
enum TorError {
    AuthenticationError(String),
    ProtocolError(String),
    IOError(std::io::Error),
}

impl From<std::io::Error> for TorError {
    fn from(error: std::io::Error) -> TorError {
        TorError::IOError(error)
    }
}

pub enum TorControlAuthentication {
    None,
    SafeCookie(String),     // Cookie String
    HashedPassword(String), // Password
}

pub struct TorControlConnectionBuilder {
    authentication: TorControlAuthentication,
    address: SocketAddr,
}

impl Default for TorControlConnectionBuilder {
    fn default() -> Self {
        Self {
            authentication: TorControlAuthentication::None,
            address: "127.0.0.1:9501".parse().unwrap(),
        }
    }
}

impl TorControlConnectionBuilder {
    fn with_authentication(mut self, auth: TorControlAuthentication) -> Self {
        self.authentication = auth;
        self
    }

    fn address<A>(mut self, addr: A) -> Self
    where
        SocketAddr: From<A>,
    {
        self.address = addr.into();
        self
    }

    async fn connect(self) -> Result<TorControlConnection<TcpStream>, TorError> {
        let connection = TcpStream::connect(self.address).await?;
        Ok(TorControlConnection {
            authentication: self.authentication,
            stream: tokio::io::BufStream::new(connection),
        })
    }

    fn mock(self, mock: Mock) -> TorControlConnection<Mock> {
        TorControlConnection {
            authentication: self.authentication,
            stream: tokio::io::BufStream::new(mock),
        }
    }
}

pub struct TorControlConnection<S: AsyncRead + AsyncWrite + Unpin> {
    authentication: TorControlAuthentication,
    stream: tokio::io::BufStream<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin + std::marker::Send> TorControlConnection<S> {
    async fn write(&mut self, data: &str) -> Result<(), TorError> {
        self.stream.write_all(data.as_bytes()).await?;
        Ok(())
    }

    async fn read_line(&mut self) -> Result<String, TorError> {
        let mut buf = String::new();
        self.stream.read_line(&mut buf).await?;
        buf = buf
            .strip_suffix("\r\n")
            .or(buf.strip_suffix("\n"))
            .unwrap_or(&buf)
            .to_string();
        Ok(buf)
    }

    async fn authenticate(&mut self) -> Result<bool, TorError> {
        match self.authentication {
            TorControlAuthentication::None => {
                self.write("AUTHENTICATE").await?;
                let (code, response) = self.parse_control_response().await?;
                match code {
                    250 => Ok(true),
                    515 => Err(TorError::AuthenticationError(response.join(" "))),
                    _ => Err(TorError::AuthenticationError(format!(
                        "Unexpected response: {} {}",
                        code,
                        response.join(" ")
                    ))),
                }
            }
            _ => Err(TorError::AuthenticationError(
                "We only currently support TorControlAuthentication::None".into(),
            )), // TODO: Implement other auth methods
        }
    }

    #[async_recursion]
    async fn parse_control_response(&mut self) -> Result<(u16, Vec<String>), TorError> {
        lazy_static! {
            static ref RE: Regex =
                Regex::new(r"^(?P<code>\d{3})(?P<type>[\- +])(?P<response>.*)\r?\n?$").unwrap();
        }

        let mut line = self.read_line().await?;
        match RE.captures(&line) {
            Some(captures) => {
                let code = captures["code"].parse::<u16>().unwrap();
                match &captures["type"] {
                    " " => Ok((code, vec![captures["response"].into()])),
                    "-" => {
                        let mut response = vec![captures["response"].into()];
                        let (_, new_response) = self.parse_control_response().await?;
                        if new_response.len() != 1 || new_response[0] != "OK" {
                            response.extend(new_response.clone());
                        }
                        Ok((code, response))
                    }
                    "+" => {
                        let mut response = vec![captures["response"].into()];
                        loop {
                            let line = self.read_line().await?;
                            if line == "." {
                                break;
                            }
                            response.push(line);
                        }
                        Ok((code, response))
                    }
                    _ => Err(TorError::ProtocolError(format!(
                        "Unexpected response type '{}': {}",
                        &captures["type"], line,
                    ))),
                }
            }
            None => Err(TorError::ProtocolError(format!(
                "Unknown response: {}",
                line
            ))),
        }
    }

    async fn create_transient_onion_service(
        &mut self,
        virt_port: u16,
        target_port: u16,
    ) -> Result<OnionService, TorError> {
        lazy_static! {
            static ref RE: Regex = Regex::new(r"^[^=]*=(?P<value>.*)$").unwrap();
        }
        self.write(&format!(
            "ADD_ONION NEW:BEST Flags=DiscardPK Port={},{}",
            virt_port, target_port
        ))
        .await?;
        let (code, response) = self.parse_control_response().await?;
        match code {
            250 => match RE.captures(&response[0]) {
                Some(captures) => Ok(OnionService {
                    virt_port,
                    target_port,
                    service_id: captures["value"].into(),
                }),
                None => Err(TorError::ProtocolError(format!(
                    "Unexpected response: {} {}",
                    code,
                    response.join(" "),
                ))),
            },
            _ => Err(TorError::ProtocolError(format!(
                "Unexpected response: {} {}",
                code,
                response.join(" "),
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_test::io::Builder;

    #[tokio::test]
    async fn parse_control_response() {
        // 250 OK response
        let mock = Builder::new().read(b"250 OK").build();
        let mut tor = TorControlConnectionBuilder::default().mock(mock);
        let result = tor.parse_control_response().await;
        assert!(result.is_ok());
        let (code, responses) = result.unwrap();
        assert_eq!(250, code);
        assert_eq!(1, responses.len());
        assert_eq!("OK", responses[0]);

        // garbled response
        let mock = Builder::new().read(b"idon'tknowwhatthisis").build();
        let mut tor = TorControlConnectionBuilder::default().mock(mock);
        let result = tor.parse_control_response().await;
        assert!(result.is_err());
        match result.err() {
            Some(TorError::ProtocolError(_)) => assert!(true),
            _ => assert!(false),
        }

        // Multiline response
        let mock = Builder::new()
            .read(b"250-ServiceID=647qjf6w3evdbdpy7oidf5vda6rsjzsl5a6ofsaou2v77hj7dmn2spqd\r\n")
            .read(b"250-PrivateKey=ED25519-V3:yLSDc8b11PaIHTtNtvi9lNW99IME2mdrO4k381zDkHv//WRUGrkBALBQ9MbHy2SLA/NmfS7YxmcR/FY8ppRfIA==\r\n")
            .read(b"250 OK")
            .build();
        let mut tor = TorControlConnectionBuilder::default().mock(mock);
        let result = tor.parse_control_response().await;
        assert!(result.is_ok());
        let (code, responses) = result.unwrap();
        assert_eq!(250, code);
        assert_eq!(2, responses.len());
        assert_eq!(
            "ServiceID=647qjf6w3evdbdpy7oidf5vda6rsjzsl5a6ofsaou2v77hj7dmn2spqd",
            responses[0]
        );
        assert_eq!("PrivateKey=ED25519-V3:yLSDc8b11PaIHTtNtvi9lNW99IME2mdrO4k381zDkHv//WRUGrkBALBQ9MbHy2SLA/NmfS7YxmcR/FY8ppRfIA==", responses[1]);

        // Data response
        let mock = Builder::new()
            .read(b"250+onions/current=\n")
            .read(b"647qjf6w3evdbdpy7oidf5vda6rsjzsl5a6ofsaou2v77hj7dmn2spqd\r\n")
            .read(b"yxq7fa63tthq3nd2ul52jjcdpblyai6k3cfmdkyw23ljsoob66z3ywid\r\n")
            .read(b".")
            .build();
        let mut tor = TorControlConnectionBuilder::default().mock(mock);
        let result = tor.parse_control_response().await;
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
    }

    #[tokio::test]
    async fn authenticate() {
        let mock = Builder::new().read(b"250 OK").build();
        let mut tor = TorControlConnectionBuilder::default().mock(mock);
        let result = tor.authenticate().await;
        assert!(result.is_ok());
        assert_eq!(true, result.unwrap());
    }
}
