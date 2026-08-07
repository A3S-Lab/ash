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

/// Quoting applied to one expansion-ready word segment.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QuoteMode {
    Unquoted,
    Single,
    Double,
}

/// One supported shell parameter reference retained before expansion.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Parameter {
    /// A named shell variable or exported environment entry.
    Variable { name: String, braced: bool },
    /// The conventional numeric status of the previously executed command.
    LastStatus,
}

impl Parameter {
    /// Returns the named parameter, or `None` for `$?`.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Variable { name, .. } => Some(name),
            Self::LastStatus => None,
        }
    }

    /// Reports whether a named parameter used `${NAME}` syntax.
    #[must_use]
    pub const fn is_braced(&self) -> bool {
        match self {
            Self::Variable { braced, .. } => *braced,
            Self::LastStatus => false,
        }
    }
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
    Parameter {
        parameter: Parameter,
        quote: QuoteMode,
        span: SourceSpan,
    },
}

impl WordPart {
    /// Returns a literal value, variable name, or `?` for the status parameter.
    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::Literal { value, .. } => value,
            Self::Parameter { parameter, .. } => parameter.name().unwrap_or("?"),
        }
    }

    /// Returns the retained parameter metadata for a parameter segment.
    #[must_use]
    pub const fn parameter(&self) -> Option<&Parameter> {
        match self {
            Self::Literal { .. } => None,
            Self::Parameter { parameter, .. } => Some(parameter),
        }
    }

    #[must_use]
    pub const fn quote(&self) -> QuoteMode {
        match self {
            Self::Literal { quote, .. } => *quote,
            Self::Parameter { quote, .. } => *quote,
        }
    }

    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        match self {
            Self::Literal { span, .. } => *span,
            Self::Parameter { span, .. } => *span,
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
        let mut value = String::new();
        for part in &self.parts {
            match part {
                WordPart::Literal { value: literal, .. } => value.push_str(literal),
                WordPart::Parameter {
                    parameter: Parameter::Variable { name, braced },
                    ..
                } => {
                    value.push('$');
                    if *braced {
                        value.push('{');
                    }
                    value.push_str(name);
                    if *braced {
                        value.push('}');
                    }
                }
                WordPart::Parameter {
                    parameter: Parameter::LastStatus,
                    ..
                } => value.push_str("$?"),
            }
        }
        value
    }
}

/// One standard descriptor that may be redirected by the current shell dialect.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RedirectionDescriptor {
    Stdin,
    Stdout,
    Stderr,
}

/// File-open behavior for one redirection target.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RedirectionFileMode {
    Read,
    Write,
    Append,
}

/// Expanded-later target of one ordered redirection.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RedirectionTarget {
    File {
        path: Word,
        mode: RedirectionFileMode,
    },
    Descriptor(RedirectionDescriptor),
}

/// One source-ordered standard-descriptor redirection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Redirection {
    descriptor: RedirectionDescriptor,
    target: RedirectionTarget,
    operator_span: SourceSpan,
    span: SourceSpan,
}

impl Redirection {
    pub(crate) const fn new(
        descriptor: RedirectionDescriptor,
        target: RedirectionTarget,
        operator_span: SourceSpan,
        span: SourceSpan,
    ) -> Self {
        Self {
            descriptor,
            target,
            operator_span,
            span,
        }
    }

    #[must_use]
    pub const fn descriptor(&self) -> RedirectionDescriptor {
        self.descriptor
    }

    #[must_use]
    pub const fn target(&self) -> &RedirectionTarget {
        &self.target
    }

    #[must_use]
    pub const fn operator_span(&self) -> SourceSpan {
        self.operator_span
    }

    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }
}

/// A foreground simple command in the currently implemented syntax subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimpleCommand {
    words: Vec<Word>,
    redirections: Vec<Redirection>,
    span: SourceSpan,
}

impl SimpleCommand {
    pub(crate) fn new(words: Vec<Word>, redirections: Vec<Redirection>, span: SourceSpan) -> Self {
        Self {
            words,
            redirections,
            span,
        }
    }

    #[must_use]
    pub fn words(&self) -> &[Word] {
        &self.words
    }

    #[must_use]
    pub fn redirections(&self) -> &[Redirection] {
        &self.redirections
    }

    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }
}

/// One foreground pipeline over a contiguous range of parsed commands.
///
/// A single command is represented as a one-stage pipeline. Pipe operator spans
/// are retained in source order and therefore number one fewer than the stages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pipeline {
    command_range: Range<usize>,
    pipe_spans: Vec<SourceSpan>,
    span: SourceSpan,
}

impl Pipeline {
    pub(crate) fn new(
        command_range: Range<usize>,
        pipe_spans: Vec<SourceSpan>,
        span: SourceSpan,
    ) -> Self {
        debug_assert!(!command_range.is_empty());
        debug_assert_eq!(pipe_spans.len() + 1, command_range.len());
        Self {
            command_range,
            pipe_spans,
            span,
        }
    }

    /// Returns the stage range in [`Script::commands`].
    #[must_use]
    pub fn command_range(&self) -> Range<usize> {
        self.command_range.clone()
    }

    #[must_use]
    pub fn pipe_spans(&self) -> &[SourceSpan] {
        &self.pipe_spans
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
    pipelines: Vec<Pipeline>,
}

impl Script {
    pub(crate) fn new(
        source: String,
        commands: Vec<SimpleCommand>,
        pipelines: Vec<Pipeline>,
    ) -> Self {
        Self {
            source,
            commands,
            pipelines,
        }
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
    pub fn pipelines(&self) -> &[Pipeline] {
        &self.pipelines
    }

    #[must_use]
    pub fn span(&self) -> SourceSpan {
        SourceSpan::new(0, self.source.len())
    }
}
