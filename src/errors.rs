use dxfilter::error::DxFilterErr;

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
    Timedout,
}

impl From<DxFilterErr> for RhinoError {
    fn from(value: DxFilterErr) -> Self {
        return Self::Unexpected(format!("{:?}", value));
    }
}