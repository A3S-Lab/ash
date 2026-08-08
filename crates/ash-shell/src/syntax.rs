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

    pub(crate) fn shifted(self, offset: usize) -> Self {
        Self::new(
            self.start
                .checked_add(offset)
                .expect("nested shell source spans fit usize"),
            self.end
                .checked_add(offset)
                .expect("nested shell source spans fit usize"),
        )
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

/// One parsed `$(...)` command substitution retained inside a word.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSubstitution {
    script: Box<Script>,
    body_span: SourceSpan,
    span: SourceSpan,
}

impl CommandSubstitution {
    pub(crate) fn new(script: Script, body_span: SourceSpan, span: SourceSpan) -> Self {
        Self {
            script: Box::new(script),
            body_span,
            span,
        }
    }

    /// Returns the parsed substitution body with spans relative to its own source.
    #[must_use]
    pub fn script(&self) -> &Script {
        &self.script
    }

    /// Returns the outer-source span of the bytes between `$(` and `)`.
    #[must_use]
    pub const fn body_span(&self) -> SourceSpan {
        self.body_span
    }

    /// Returns the outer-source span covering `$(`, its body, and `)`.
    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
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
    CommandSubstitution {
        substitution: CommandSubstitution,
        quote: QuoteMode,
    },
}

impl WordPart {
    /// Returns a literal value, parameter name/status marker, or substitution body source.
    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::Literal { value, .. } => value,
            Self::Parameter { parameter, .. } => parameter.name().unwrap_or("?"),
            Self::CommandSubstitution { substitution, .. } => substitution.script().source(),
        }
    }

    /// Returns the retained parameter metadata for a parameter segment.
    #[must_use]
    pub const fn parameter(&self) -> Option<&Parameter> {
        match self {
            Self::Literal { .. } | Self::CommandSubstitution { .. } => None,
            Self::Parameter { parameter, .. } => Some(parameter),
        }
    }

    /// Returns the parsed command substitution for this segment, when present.
    #[must_use]
    pub const fn command_substitution(&self) -> Option<&CommandSubstitution> {
        match self {
            Self::CommandSubstitution { substitution, .. } => Some(substitution),
            Self::Literal { .. } | Self::Parameter { .. } => None,
        }
    }

    #[must_use]
    pub const fn quote(&self) -> QuoteMode {
        match self {
            Self::Literal { quote, .. } => *quote,
            Self::Parameter { quote, .. } => *quote,
            Self::CommandSubstitution { quote, .. } => *quote,
        }
    }

    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        match self {
            Self::Literal { span, .. } => *span,
            Self::Parameter { span, .. } => *span,
            Self::CommandSubstitution { substitution, .. } => substitution.span(),
        }
    }

    fn shift_for_execution(&mut self, offset: usize) {
        match self {
            Self::Literal { span, .. } | Self::Parameter { span, .. } => {
                *span = span.shifted(offset);
            }
            Self::CommandSubstitution { substitution, .. } => {
                substitution.body_span = substitution.body_span.shifted(offset);
                substitution.span = substitution.span.shifted(offset);
            }
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
                WordPart::CommandSubstitution { substitution, .. } => {
                    value.push_str("$(");
                    value.push_str(substitution.script().source());
                    value.push(')');
                }
            }
        }
        value
    }

    fn shift_for_execution(&mut self, offset: usize) {
        self.span = self.span.shifted(offset);
        for part in &mut self.parts {
            part.shift_for_execution(offset);
        }
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

    fn shift_for_execution(&mut self, offset: usize) {
        if let RedirectionTarget::File { path, .. } = &mut self.target {
            path.shift_for_execution(offset);
        }
        self.operator_span = self.operator_span.shifted(offset);
        self.span = self.span.shifted(offset);
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

    fn shift_for_execution(&mut self, offset: usize) {
        for word in &mut self.words {
            word.shift_for_execution(offset);
        }
        for redirection in &mut self.redirections {
            redirection.shift_for_execution(offset);
        }
        self.span = self.span.shifted(offset);
    }
}

/// One operator in a left-associative conditional pipeline list.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConditionalOperator {
    /// Execute the following pipeline only when the preceding status is zero.
    AndIf,
    /// Execute the following pipeline only when the preceding status is nonzero.
    OrIf,
}

/// The source-spanned conditional operator that gates one following pipeline.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PipelineCondition {
    operator: ConditionalOperator,
    span: SourceSpan,
}

impl PipelineCondition {
    pub(crate) const fn new(operator: ConditionalOperator, span: SourceSpan) -> Self {
        Self { operator, span }
    }

    #[must_use]
    pub const fn operator(self) -> ConditionalOperator {
        self.operator
    }

    #[must_use]
    pub const fn span(self) -> SourceSpan {
        self.span
    }

    fn shift_for_execution(&mut self, offset: usize) {
        self.span = self.span.shifted(offset);
    }
}

/// One foreground pipeline over a contiguous range of parsed commands.
///
/// A single command is represented as a one-stage pipeline. Pipe operator spans
/// are retained in source order and therefore number one fewer than the stages.
/// A conditional link, when present, gates this pipeline with the status of the
/// preceding pipeline in the same left-associative AND-OR list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pipeline {
    command_range: Range<usize>,
    pipe_spans: Vec<SourceSpan>,
    condition: Option<PipelineCondition>,
    span: SourceSpan,
}

impl Pipeline {
    pub(crate) fn new(
        command_range: Range<usize>,
        pipe_spans: Vec<SourceSpan>,
        condition: Option<PipelineCondition>,
        span: SourceSpan,
    ) -> Self {
        debug_assert!(!command_range.is_empty());
        debug_assert_eq!(pipe_spans.len() + 1, command_range.len());
        Self {
            command_range,
            pipe_spans,
            condition,
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

    /// Returns the conditional operator that must admit this pipeline.
    #[must_use]
    pub const fn condition(&self) -> Option<PipelineCondition> {
        self.condition
    }

    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }

    fn shift_for_execution(&mut self, offset: usize) {
        for span in &mut self.pipe_spans {
            *span = span.shifted(offset);
        }
        if let Some(condition) = &mut self.condition {
            condition.shift_for_execution(offset);
        }
        self.span = self.span.shifted(offset);
    }
}

/// A parsed script retaining its exact original source and byte spans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Script {
    source: String,
    commands: Vec<SimpleCommand>,
    pipelines: Vec<Pipeline>,
    span: SourceSpan,
}

impl Script {
    pub(crate) fn new(
        source: String,
        commands: Vec<SimpleCommand>,
        pipelines: Vec<Pipeline>,
    ) -> Self {
        let span = SourceSpan::new(0, source.len());
        Self {
            source,
            commands,
            pipelines,
            span,
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
        self.span
    }

    pub(crate) fn shifted_for_execution(&self, offset: usize) -> Self {
        let mut shifted = self.clone();
        shifted.shift_for_execution(offset);
        shifted
    }

    fn shift_for_execution(&mut self, offset: usize) {
        for command in &mut self.commands {
            command.shift_for_execution(offset);
        }
        for pipeline in &mut self.pipelines {
            pipeline.shift_for_execution(offset);
        }
        self.span = self.span.shifted(offset);
    }
}
