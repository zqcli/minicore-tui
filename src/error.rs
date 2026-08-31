use std::fmt;
use std::io;

/// An I/O failure inside a terminal lifecycle step.
#[derive(Debug)]
pub struct TerminalError {
    operation: &'static str,
    source: io::Error,
}

impl TerminalError {
    pub fn new(operation: &'static str, source: io::Error) -> Self {
        Self { operation, source }
    }
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.operation, self.source)
    }
}

impl std::error::Error for TerminalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn reports_operation_and_source() {
        let error = TerminalError::new(
            "enter alternate screen",
            io::Error::new(io::ErrorKind::Other, "boom"),
        );
        let message = error.to_string();
        assert!(message.contains("enter alternate screen"));
        assert!(message.contains("boom"));
        assert_eq!(error.source().unwrap().to_string(), "boom");
    }
}
