/// Why a manifest was refused. Messages are shown to the user, so they say what is wrong
/// with the source, never what internal structure was involved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("the manifest is larger than {limit} bytes")]
    TooLarge { limit: usize },
    #[error("the manifest has more than {limit} {what}")]
    TooMany { what: &'static str, limit: usize },
    #[error("this isn't a valid manifest: {0}")]
    Invalid(String),
    #[error("K-Star can't handle this kind of stream yet: {0}")]
    Unsupported(String),
    #[error("manifests with DTD declarations are refused")]
    Doctype,
}

impl ParseError {
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        ParseError::Invalid(msg.into())
    }

    pub(crate) fn unsupported(msg: impl Into<String>) -> Self {
        ParseError::Unsupported(msg.into())
    }
}
