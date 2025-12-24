use std::io;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Network stack error: {0}")]
    Stack(String),

    #[error("Connection error: {0}")]
    Connection(String),
}
