use std::fmt;

/// Everything that can stop a decode. A malformed or hostile file always ends
/// in one of these; it never panics. A panic is a bug in this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A read ran past the end of its buffer (a box, an OBU, a header).
    Truncated(&'static str),
    /// The bytes break AV2's syntax, or a constraint AVM enforces.
    Invalid(&'static str),
    /// Valid AV2 that this decoder doesn't read yet. It covers what this repo's
    /// encoder writes at speed 6 and refuses the rest by name.
    Unsupported(&'static str),
    /// The container holds no AV2 image item.
    NotAv2,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated(what) => write!(f, "truncated: {what}"),
            Error::Invalid(what) => write!(f, "invalid AV2: {what}"),
            Error::Unsupported(what) => write!(f, "not supported yet: {what}"),
            Error::NotAv2 => write!(f, "no AV2 (av02) primary image item"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
