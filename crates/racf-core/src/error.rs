use std::fmt;

/// Errors produced while reading a RACF document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RacfError {
    /// The underlying XML could not be parsed.
    Xml(String),
    /// The root element was not `<racf>`.
    InvalidRoot(String),
    /// An attribute had a value RACF does not understand.
    InvalidAttribute {
        element: String,
        attribute: String,
        value: String,
    },
    /// A channel layout was empty or contained duplicates.
    InvalidChannelLayout,
    /// An embedded audio clip was malformed.
    InvalidAudio { clip: String, message: String },
}

impl fmt::Display for RacfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RacfError::Xml(message) => write!(f, "invalid RACF XML: {message}"),
            RacfError::InvalidRoot(root) => {
                write!(f, "expected <racf> root element, found <{root}>")
            }
            RacfError::InvalidAttribute {
                element,
                attribute,
                value,
            } => write!(
                f,
                "invalid value {value:?} for {attribute:?} on <{element}>"
            ),
            RacfError::InvalidChannelLayout => {
                write!(
                    f,
                    "a RACF channel layout must be non-empty and contain no duplicates"
                )
            }
            RacfError::InvalidAudio { clip, message } => {
                write!(f, "invalid audio clip {clip:?}: {message}")
            }
        }
    }
}

impl std::error::Error for RacfError {}

impl From<roxmltree::Error> for RacfError {
    fn from(error: roxmltree::Error) -> Self {
        RacfError::Xml(error.to_string())
    }
}
