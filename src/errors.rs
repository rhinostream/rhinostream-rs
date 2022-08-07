pub type RhinoResult<T> = Result<T, RhinoError>;

#[derive(Clone, Debug)]
pub enum RhinoError {
    UnSupported,
    NotFound,
    Exiting,
    Recoverable(String),
    UnRecoverable(String),
    ParseError(String),
    Unexpected(String),
}