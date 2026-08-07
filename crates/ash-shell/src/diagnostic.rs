use std::fmt;

use crate::SourceSpan;

/// Stable category for a source-spanned shell parse diagnostic.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum DiagnosticCode {
    UnexpectedSeparator,
    UnsupportedSyntax,
    UnterminatedSingleQuote,
    UnterminatedDoubleQuote,
    UnterminatedParameterExpansion,
    InvalidParameterExpansion,
    TrailingEscape,
}

/// A parse diagnostic anchored to the exact submitted UTF-8 bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    code: DiagnosticCode,
    message: &'static str,
    span: SourceSpan,
}

impl Diagnostic {
    pub(crate) const fn new(code: DiagnosticCode, message: &'static str, span: SourceSpan) -> Self {
        Self {
            code,
            message,
            span,
        }
    }

    #[must_use]
    pub const fn code(&self) -> DiagnosticCode {
        self.code
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }

    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at bytes {}..{}",
            self.message,
            self.span.start(),
            self.span.end()
        )
    }
}

impl std::error::Error for Diagnostic {}
