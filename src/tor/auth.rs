use crate::tor::control_connection::TorControlConnection;
use crate::tor::error::TorError;
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};

pub enum TorAuthentication {
    Null,
    SafeCookie(String),     // Cookie String
    HashedPassword(String), // Password
}

impl TorAuthentication {
    pub async fn authenticate(
        &self,
        connection: &mut TorControlConnection,
    ) -> Result<(), TorError> {
        match self {
            TorAuthentication::Null => match connection.send_command("AUTHENTICATE", None).await {
                Ok(_) => Ok(()),
                Err(TorError::ProtocolError(error)) => Err(TorError::AuthenticationError(error)),
                Err(error) => Err(error),
            },
            _ => Err(TorError::authentication_error(
                "Haven't implemented that authentication method yet",
            )),
        }
    }
}
