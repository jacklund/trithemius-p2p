use tokio::sync::oneshot;
use tokio_util::codec::LinesCodecError;

#[derive(Debug, PartialEq, Eq)]
pub enum TorError {
    AuthenticationError(String),
    ProtocolError(String),
    IOError(String),
}

impl std::fmt::Display for TorError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::AuthenticationError(error) => write!(f, "Authentication Error: {}", error),
            Self::ProtocolError(error) => write!(f, "Protocol Error: {}", error),
            Self::IOError(error) => write!(f, "IO Error: {}", error),
        }
    }
}

impl std::error::Error for TorError {}

impl TorError {
    pub fn authentication_error(msg: &str) -> TorError {
        TorError::AuthenticationError(msg.to_string())
    }

    pub fn protocol_error(msg: &str) -> TorError {
        TorError::ProtocolError(msg.to_string())
    }
}

impl From<std::io::Error> for TorError {
    fn from(error: std::io::Error) -> TorError {
        TorError::IOError(error.to_string())
    }
}

impl From<oneshot::error::RecvError> for TorError {
    fn from(error: oneshot::error::RecvError) -> TorError {
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
