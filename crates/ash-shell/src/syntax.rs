use std::ops::Range;

/// A half-open UTF-8 byte range in the submitted source text.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceSpan {
    start: usize,
    end: usize,
}

impl SourceSpan {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        assert!(start <= end, "source span start must not exceed its end");
        Self { start, end }
    }

    #[must_use]
    pub const fn start(self) -> usize {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> usize {
        self.end
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    #[must_use]
    pub fn range(self) -> Range<usize> {
        self.start..self.end
    }

    #[must_use]
    pub fn source_text(self, source: &str) -> Option<&str> {
        source.get(self.range())
    }
}

/// Quoting applied to one literal word segment.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QuoteMode {
    Unquoted,
    Single,
    Double,
}

/// One expansion-ready segment of a shell word.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WordPart {
    Literal {
        value: String,
        quote: QuoteMode,
        span: SourceSpan,
    },
}

impl WordPart {
    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::Literal { value, .. } => value,
        }
    }

    #[must_use]
    pub const fn quote(&self) -> QuoteMode {
        match self {
            Self::Literal { quote, .. } => *quote,
        }
    }

    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        match self {
            Self::Literal { span, .. } => *span,
        }
    }
}

/// A shell word whose quoted and unquoted segments remain distinct.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Word {
    parts: Vec<WordPart>,
    span: SourceSpan,
}

impl Word {
    pub(crate) fn new(parts: Vec<WordPart>, span: SourceSpan) -> Self {
        Self { parts, span }
    }

    #[must_use]
    pub fn parts(&self) -> &[WordPart] {
        &self.parts
    }

    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }

    /// Returns the literal value before parameter, command, or glob expansion.
    #[must_use]
    pub fn literal(&self) -> String {
        let length = self.parts.iter().map(|part| part.value().len()).sum();
        let mut value = String::with_capacity(length);
        for part in &self.parts {
            value.push_str(part.value());
        }
        value
    }
}

/// A foreground simple command in the H0 syntax subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimpleCommand {
    words: Vec<Word>,
    span: SourceSpan,
}

impl SimpleCommand {
    pub(crate) fn new(words: Vec<Word>, span: SourceSpan) -> Self {
        Self { words, span }
    }

    #[must_use]
    pub fn words(&self) -> &[Word] {
        &self.words
    }

    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }
}

/// A parsed script retaining its exact original source and byte spans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Script {
    source: String,
    commands: Vec<SimpleCommand>,
}

impl Script {
    pub(crate) fn new(source: String, commands: Vec<SimpleCommand>) -> Self {
        Self { source, commands }
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn commands(&self) -> &[SimpleCommand] {
        &self.commands
    }

    #[must_use]
    pub fn span(&self) -> SourceSpan {
        SourceSpan::new(0, self.source.len())
    }
}
