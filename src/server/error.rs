use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Core(#[from] crate::error::Error),
}

pub type Result<T> = std::result::Result<T, ServerError>;
