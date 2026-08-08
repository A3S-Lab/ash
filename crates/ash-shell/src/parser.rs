use crate::{
    CommandSubstitution, ConditionalOperator, Diagnostic, DiagnosticCode, Parameter, Pipeline,
    PipelineCondition, QuoteMode, Redirection, RedirectionDescriptor, RedirectionFileMode,
    RedirectionTarget, Script, SimpleCommand, SourceSpan, Word, WordPart,
};

/// Maximum recursively parsed `$(...)` nesting accepted by one source.
pub const MAX_COMMAND_SUBSTITUTION_DEPTH: usize = 32;

/// Parses the currently implemented human-shell syntax subset.
///
/// The current subset accepts foreground native-pipeline syntax with `|`,
/// left-associative `&&`/`||` conditional lists, source-ordered file and
/// descriptor redirections, simple commands separated by a newline or `;`,
/// shell comments, single and double quotes, backslash escaping, named
/// parameters, `$?`, and recursively parsed `$(...)` command substitutions.
/// Syntax reserved for later milestones is rejected with a source span instead
/// of being reinterpreted as a literal argument.
pub fn parse(source: &str) -> Result<Script, Diagnostic> {
    Parser::new(source).parse()
}

struct Parser<'a> {
    source: &'a str,
    position: usize,
    command_substitution_depth: usize,
}

impl<'a> Parser<'a> {
    const fn new(source: &'a str) -> Self {
        Self::with_command_substitution_depth(source, 0)
    }

    const fn with_command_substitution_depth(
        source: &'a str,
        command_substitution_depth: usize,
    ) -> Self {
        Self {
            source,
            position: 0,
            command_substitution_depth,
        }
    }

    fn parse(mut self) -> Result<Script, Diagnostic> {
        let mut commands = Vec::new();
        let mut pipelines = Vec::new();
        let mut condition = None;
        self.skip_command_layout();

        while !self.is_eof() {
            if self.starts_conditional_operator() {
                return Err(self.conditional_separator_diagnostic(
                    "a conditional operator must follow a pipeline",
                ));
            }
            if self.peek() == Some(';') {
                return Err(self.diagnostic_here(
                    DiagnosticCode::UnexpectedSeparator,
                    "a command separator must follow a command",
                ));
            }

            let pipeline_start = self.position;
            let first_command = commands.len();
            let mut pipe_spans = Vec::new();
            let pipeline_end;
            loop {
                if self.peek() == Some('|') {
                    return Err(self.diagnostic_here(
                        DiagnosticCode::UnexpectedSeparator,
                        "a pipeline operator must follow a command",
                    ));
                }

                let command_start = self.position;
                let mut words = Vec::new();
                let mut redirections = Vec::new();
                let mut command_end = None;
                loop {
                    let separated = self.skip_horizontal();
                    match self.peek() {
                        None | Some('\n' | ';' | '|') => break,
                        Some('&') if self.starts_conditional_operator() => break,
                        Some('#') if separated => break,
                        Some(_) if self.starts_redirection() => {
                            let redirection = self.parse_redirection()?;
                            command_end = Some(redirection.span().end());
                            redirections.push(redirection);
                        }
                        Some(_) => {
                            let word = self.parse_word()?;
                            command_end = Some(word.span().end());
                            words.push(word);
                        }
                    }
                }

                let Some(command_end) = command_end.filter(|_| !words.is_empty()) else {
                    return Err(self.diagnostic_here(
                        DiagnosticCode::UnexpectedSeparator,
                        "a pipeline stage must contain a command",
                    ));
                };
                commands.push(SimpleCommand::new(
                    words,
                    redirections,
                    SourceSpan::new(command_start, command_end),
                ));

                self.skip_horizontal();
                if self.peek() == Some('#') {
                    self.skip_comment();
                    pipeline_end = command_end;
                    break;
                }
                match self.peek() {
                    Some('|') if self.starts_conditional_operator() => {
                        pipeline_end = command_end;
                        break;
                    }
                    Some('&') if self.starts_conditional_operator() => {
                        pipeline_end = command_end;
                        break;
                    }
                    Some('|') => {
                        let pipe_start = self.position;
                        self.bump();
                        let pipe_span = SourceSpan::new(pipe_start, self.position);
                        pipe_spans.push(pipe_span);
                        self.skip_horizontal();
                        if matches!(self.peek(), None | Some('\n' | ';' | '|' | '#'))
                            || self.starts_conditional_operator()
                        {
                            return Err(Diagnostic::new(
                                DiagnosticCode::UnexpectedSeparator,
                                "a pipeline operator must be followed by a command on the same line",
                                pipe_span,
                            ));
                        }
                    }
                    None | Some('\n' | ';') => {
                        pipeline_end = command_end;
                        break;
                    }
                    Some(_) => {
                        return Err(self.diagnostic_here(
                            DiagnosticCode::UnsupportedSyntax,
                            "unsupported syntax follows a pipeline stage",
                        ));
                    }
                }
            }

            pipelines.push(Pipeline::new(
                first_command..commands.len(),
                pipe_spans,
                condition.take(),
                SourceSpan::new(pipeline_start, pipeline_end),
            ));

            self.skip_horizontal();
            if self.starts_conditional_operator() {
                let operator_start = self.position;
                let operator = if self.source[self.position..].starts_with("&&") {
                    ConditionalOperator::AndIf
                } else {
                    ConditionalOperator::OrIf
                };
                self.bump();
                self.bump();
                let operator_span = SourceSpan::new(operator_start, self.position);
                condition = Some(PipelineCondition::new(operator, operator_span));
                self.skip_conditional_layout();
                if self.is_eof()
                    || self.peek() == Some(';')
                    || self.starts_conditional_operator()
                    || self.peek() == Some('|')
                {
                    return Err(Diagnostic::new(
                        DiagnosticCode::UnexpectedSeparator,
                        "a conditional operator must be followed by a pipeline",
                        operator_span,
                    ));
                }
                continue;
            }

            match self.peek() {
                None => break,
                Some('\n' | ';') => {
                    self.bump();
                    condition = None;
                    self.skip_command_layout();
                }
                Some(_) => {
                    return Err(self.diagnostic_here(
                        DiagnosticCode::UnsupportedSyntax,
                        "unsupported syntax follows a pipeline",
                    ));
                }
            }
        }

        Ok(Script::new(self.source.to_owned(), commands, pipelines))
    }

    fn starts_redirection(&self) -> bool {
        let mut characters = self.source[self.position..].chars().peekable();
        match characters.peek().copied() {
            Some('<' | '>') => true,
            Some(character) if character.is_ascii_digit() => {
                while characters
                    .peek()
                    .is_some_and(|character| character.is_ascii_digit())
                {
                    characters.next();
                }
                matches!(characters.next(), Some('<' | '>'))
            }
            _ => false,
        }
    }

    fn parse_redirection(&mut self) -> Result<Redirection, Diagnostic> {
        let redirection_start = self.position;
        let descriptor = self.parse_redirection_descriptor_prefix()?;
        let operator = self
            .bump()
            .expect("a detected redirection retains its operator");

        match operator {
            '<' => {
                if self.peek() == Some('<') {
                    self.bump();
                    return Err(Diagnostic::new(
                        DiagnosticCode::UnsupportedSyntax,
                        "here documents are reserved for a later milestone",
                        SourceSpan::new(redirection_start, self.position),
                    ));
                }
                let descriptor = match descriptor {
                    None | Some(0) => RedirectionDescriptor::Stdin,
                    Some(_) => {
                        return Err(Diagnostic::new(
                            DiagnosticCode::InvalidRedirection,
                            "input redirection supports only descriptor 0",
                            SourceSpan::new(redirection_start, self.position),
                        ));
                    }
                };
                let operator_span = SourceSpan::new(redirection_start, self.position);
                self.parse_file_redirection(
                    redirection_start,
                    operator_span,
                    descriptor,
                    RedirectionFileMode::Read,
                )
            }
            '>' => {
                let append = self.peek() == Some('>');
                if append {
                    self.bump();
                }
                let descriptor = match descriptor {
                    None | Some(1) => RedirectionDescriptor::Stdout,
                    Some(2) => RedirectionDescriptor::Stderr,
                    Some(_) => {
                        return Err(Diagnostic::new(
                            DiagnosticCode::InvalidRedirection,
                            "output redirection supports only descriptors 1 and 2",
                            SourceSpan::new(redirection_start, self.position),
                        ));
                    }
                };
                if !append && self.peek() == Some('&') {
                    self.bump();
                    return self.parse_descriptor_redirection(redirection_start, descriptor);
                }
                if append && self.peek() == Some('&') {
                    self.bump();
                    return Err(Diagnostic::new(
                        DiagnosticCode::InvalidRedirection,
                        "append redirection cannot duplicate a descriptor",
                        SourceSpan::new(redirection_start, self.position),
                    ));
                }
                let operator_span = SourceSpan::new(redirection_start, self.position);
                self.parse_file_redirection(
                    redirection_start,
                    operator_span,
                    descriptor,
                    if append {
                        RedirectionFileMode::Append
                    } else {
                        RedirectionFileMode::Write
                    },
                )
            }
            _ => unreachable!("a redirection operator is either input or output"),
        }
    }

    fn parse_redirection_descriptor_prefix(&mut self) -> Result<Option<u8>, Diagnostic> {
        let start = self.position;
        while self
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            self.bump();
        }
        if self.position == start {
            return Ok(None);
        }
        self.source[start..self.position]
            .parse::<u8>()
            .map(Some)
            .map_err(|_| {
                Diagnostic::new(
                    DiagnosticCode::InvalidRedirection,
                    "redirection descriptor is outside the supported range",
                    SourceSpan::new(start, self.position),
                )
            })
    }

    fn parse_file_redirection(
        &mut self,
        redirection_start: usize,
        operator_span: SourceSpan,
        descriptor: RedirectionDescriptor,
        mode: RedirectionFileMode,
    ) -> Result<Redirection, Diagnostic> {
        let separated = self.skip_horizontal();
        if matches!(self.peek(), None | Some('\n' | ';' | '|' | '<' | '>'))
            || self.starts_conditional_operator()
            || (separated && self.peek() == Some('#'))
        {
            return Err(Diagnostic::new(
                DiagnosticCode::UnexpectedSeparator,
                "a file redirection operator must be followed by a target",
                operator_span,
            ));
        }
        let path = self.parse_word()?;
        let span = SourceSpan::new(redirection_start, path.span().end());
        Ok(Redirection::new(
            descriptor,
            RedirectionTarget::File { path, mode },
            operator_span,
            span,
        ))
    }

    fn parse_descriptor_redirection(
        &mut self,
        redirection_start: usize,
        descriptor: RedirectionDescriptor,
    ) -> Result<Redirection, Diagnostic> {
        let target = match self.bump() {
            Some('1') => RedirectionDescriptor::Stdout,
            Some('2') => RedirectionDescriptor::Stderr,
            Some(_) | None => {
                return Err(Diagnostic::new(
                    DiagnosticCode::InvalidRedirection,
                    "descriptor duplication supports only descriptors 1 and 2",
                    SourceSpan::new(redirection_start, self.position),
                ));
            }
        };
        if self.peek().is_some_and(|character| {
            !matches!(
                character,
                ' ' | '\t' | '\r' | '\n' | ';' | '|' | '&' | '<' | '>'
            )
        }) {
            return Err(Diagnostic::new(
                DiagnosticCode::InvalidRedirection,
                "a duplicated descriptor must end at an operator or word boundary",
                SourceSpan::new(redirection_start, self.position),
            ));
        }
        let span = SourceSpan::new(redirection_start, self.position);
        Ok(Redirection::new(
            descriptor,
            RedirectionTarget::Descriptor(target),
            span,
            span,
        ))
    }

    fn parse_word(&mut self) -> Result<Word, Diagnostic> {
        let word_start = self.position;
        let mut parts = Vec::new();
        let mut unquoted = String::new();
        let mut unquoted_start = None;

        while let Some(character) = self.peek() {
            if matches!(character, ' ' | '\t' | '\r' | '\n' | ';') {
                break;
            }
            if matches!(character, '|' | '<' | '>') {
                break;
            }
            if self.starts_conditional_operator() {
                break;
            }
            if is_reserved_operator(character) {
                return Err(self.diagnostic_here(
                    DiagnosticCode::UnsupportedSyntax,
                    "background jobs and subshells are reserved for a later milestone",
                ));
            }
            match character {
                '\'' => {
                    push_unquoted(
                        &mut parts,
                        &mut unquoted,
                        &mut unquoted_start,
                        self.position,
                    );
                    parts.push(self.parse_single_quoted()?);
                }
                '"' => {
                    push_unquoted(
                        &mut parts,
                        &mut unquoted,
                        &mut unquoted_start,
                        self.position,
                    );
                    parts.extend(self.parse_double_quoted()?);
                }
                '$' => {
                    push_unquoted(
                        &mut parts,
                        &mut unquoted,
                        &mut unquoted_start,
                        self.position,
                    );
                    if self.starts_command_substitution() {
                        parts.push(self.parse_command_substitution(QuoteMode::Unquoted)?);
                    } else {
                        let dollar_start = self.position;
                        if let Some(parameter) = self.parse_parameter(QuoteMode::Unquoted)? {
                            parts.push(parameter);
                        } else {
                            unquoted_start = Some(dollar_start);
                            unquoted.push('$');
                        }
                    }
                }
                '\\' => {
                    unquoted_start.get_or_insert(self.position);
                    let escape_start = self.position;
                    self.bump();
                    let Some(escaped) = self.bump() else {
                        return Err(Diagnostic::new(
                            DiagnosticCode::TrailingEscape,
                            "a trailing backslash must escape another character",
                            SourceSpan::new(escape_start, self.source.len()),
                        ));
                    };
                    if escaped != '\n' {
                        unquoted.push(escaped);
                    }
                }
                _ => {
                    unquoted_start.get_or_insert(self.position);
                    unquoted.push(character);
                    self.bump();
                }
            }
        }

        push_unquoted(
            &mut parts,
            &mut unquoted,
            &mut unquoted_start,
            self.position,
        );
        Ok(Word::new(parts, SourceSpan::new(word_start, self.position)))
    }

    fn parse_single_quoted(&mut self) -> Result<WordPart, Diagnostic> {
        let quote_start = self.position;
        self.bump();
        let mut value = String::new();
        loop {
            match self.bump() {
                Some('\'') => {
                    return Ok(WordPart::Literal {
                        value,
                        quote: QuoteMode::Single,
                        span: SourceSpan::new(quote_start, self.position),
                    });
                }
                Some(character) => value.push(character),
                None => {
                    return Err(Diagnostic::new(
                        DiagnosticCode::UnterminatedSingleQuote,
                        "single-quoted text is missing its closing quote",
                        SourceSpan::new(quote_start, self.source.len()),
                    ));
                }
            }
        }
    }

    fn parse_double_quoted(&mut self) -> Result<Vec<WordPart>, Diagnostic> {
        let quote_start = self.position;
        self.bump();
        let mut parts = Vec::new();
        let mut value = String::new();
        let mut literal_start = quote_start;
        loop {
            match self.peek() {
                Some('"') => {
                    self.bump();
                    if !value.is_empty() || parts.is_empty() {
                        parts.push(WordPart::Literal {
                            value,
                            quote: QuoteMode::Double,
                            span: SourceSpan::new(literal_start, self.position),
                        });
                    }
                    return Ok(parts);
                }
                Some('$') => {
                    let dollar_start = self.position;
                    let expansion = if self.starts_command_substitution() {
                        Some(self.parse_command_substitution(QuoteMode::Double)?)
                    } else {
                        self.parse_parameter(QuoteMode::Double)?
                    };
                    if let Some(expansion) = expansion {
                        if !value.is_empty() {
                            parts.push(WordPart::Literal {
                                value: std::mem::take(&mut value),
                                quote: QuoteMode::Double,
                                span: SourceSpan::new(literal_start, dollar_start),
                            });
                        }
                        parts.push(expansion);
                        literal_start = self.position;
                    } else {
                        value.push('$');
                    }
                }
                Some('\\') => {
                    let escape_start = self.position;
                    self.bump();
                    let Some(escaped) = self.bump() else {
                        return Err(Diagnostic::new(
                            DiagnosticCode::TrailingEscape,
                            "a trailing backslash must escape another character",
                            SourceSpan::new(escape_start, self.source.len()),
                        ));
                    };
                    match escaped {
                        '"' | '\\' | '$' => value.push(escaped),
                        '\n' => {}
                        _ => {
                            value.push('\\');
                            value.push(escaped);
                        }
                    }
                }
                Some(character) => {
                    value.push(character);
                    self.bump();
                }
                None => {
                    return Err(Diagnostic::new(
                        DiagnosticCode::UnterminatedDoubleQuote,
                        "double-quoted text is missing its closing quote",
                        SourceSpan::new(quote_start, self.source.len()),
                    ));
                }
            }
        }
    }

    fn parse_command_substitution(&mut self, quote: QuoteMode) -> Result<WordPart, Diagnostic> {
        let substitution_start = self.position;
        debug_assert!(self.starts_command_substitution());
        if self.source[substitution_start..].starts_with("$((") {
            return Err(Diagnostic::new(
                DiagnosticCode::InvalidParameterExpansion,
                "arithmetic expansion is not implemented",
                SourceSpan::new(substitution_start, substitution_start + 3),
            ));
        }
        if self.command_substitution_depth >= MAX_COMMAND_SUBSTITUTION_DEPTH {
            return Err(Diagnostic::new(
                DiagnosticCode::CommandSubstitutionDepthExceeded,
                "command substitution nesting exceeds the 32-level limit",
                SourceSpan::new(substitution_start, substitution_start + 2),
            ));
        }
        self.bump();
        self.bump();
        let body_start = self.position;
        let body_end = self.find_command_substitution_end(substitution_start)?;
        let body = &self.source[body_start..body_end];
        let script =
            Self::with_command_substitution_depth(body, self.command_substitution_depth + 1)
                .parse()
                .map_err(|diagnostic| diagnostic.shifted(body_start))?;
        if script.commands().is_empty() {
            return Err(Diagnostic::new(
                DiagnosticCode::UnexpectedSeparator,
                "a command substitution must contain a command",
                SourceSpan::new(substitution_start, substitution_start + 2),
            ));
        }
        self.position = body_end;
        debug_assert_eq!(self.bump(), Some(')'));
        let span = SourceSpan::new(substitution_start, self.position);
        Ok(WordPart::CommandSubstitution {
            substitution: CommandSubstitution::new(
                script,
                SourceSpan::new(body_start, body_end),
                span,
            ),
            quote,
        })
    }

    fn find_command_substitution_end(
        &self,
        substitution_start: usize,
    ) -> Result<usize, Diagnostic> {
        let mut position = substitution_start + 2;
        let mut frames = vec![CommandSubstitutionScanFrame::new()];
        while position < self.source.len() {
            let character = self.source[position..]
                .chars()
                .next()
                .expect("a non-EOF parser position starts a UTF-8 character");
            let character_end = position + character.len_utf8();
            let frame = frames
                .last_mut()
                .expect("one command-substitution scan frame remains active");

            if frame.comment {
                position = character_end;
                if character == '\n' {
                    frame.comment = false;
                    frame.comment_boundary = true;
                }
                continue;
            }

            match frame.quote {
                CommandSubstitutionScanQuote::Single => {
                    position = character_end;
                    if character == '\'' {
                        frame.quote = CommandSubstitutionScanQuote::Unquoted;
                    }
                }
                CommandSubstitutionScanQuote::Double => {
                    if character == '\\' {
                        position = character_end;
                        if position < self.source.len() {
                            position += self.source[position..]
                                .chars()
                                .next()
                                .expect("escaped UTF-8 character")
                                .len_utf8();
                        }
                    } else if character == '"' {
                        frame.quote = CommandSubstitutionScanQuote::Unquoted;
                        position = character_end;
                    } else if self.source[position..].starts_with("$(") {
                        self.push_command_substitution_scan_frame(&mut frames, position)?;
                        position += 2;
                    } else {
                        position = character_end;
                    }
                }
                CommandSubstitutionScanQuote::Unquoted => {
                    if character == '\\' {
                        position = character_end;
                        if position < self.source.len() {
                            position += self.source[position..]
                                .chars()
                                .next()
                                .expect("escaped UTF-8 character")
                                .len_utf8();
                        }
                        frame.comment_boundary = false;
                    } else if character == '\'' {
                        frame.quote = CommandSubstitutionScanQuote::Single;
                        frame.comment_boundary = false;
                        position = character_end;
                    } else if character == '"' {
                        frame.quote = CommandSubstitutionScanQuote::Double;
                        frame.comment_boundary = false;
                        position = character_end;
                    } else if character == '#' && frame.comment_boundary {
                        frame.comment = true;
                        position = character_end;
                    } else if self.source[position..].starts_with("$(") {
                        self.push_command_substitution_scan_frame(&mut frames, position)?;
                        position += 2;
                    } else if character == ')' {
                        if frames.len() == 1 {
                            return Ok(position);
                        }
                        frames.pop();
                        frames
                            .last_mut()
                            .expect("a parent substitution scan frame remains")
                            .comment_boundary = false;
                        position = character_end;
                    } else {
                        frame.comment_boundary =
                            character.is_ascii_whitespace() || matches!(character, ';' | '|' | '&');
                        position = character_end;
                    }
                }
            }
        }

        Err(Diagnostic::new(
            DiagnosticCode::UnterminatedCommandSubstitution,
            "command substitution is missing its closing parenthesis",
            SourceSpan::new(substitution_start, substitution_start + 2),
        ))
    }

    fn push_command_substitution_scan_frame(
        &self,
        frames: &mut Vec<CommandSubstitutionScanFrame>,
        position: usize,
    ) -> Result<(), Diagnostic> {
        if self.command_substitution_depth + frames.len() >= MAX_COMMAND_SUBSTITUTION_DEPTH {
            return Err(Diagnostic::new(
                DiagnosticCode::CommandSubstitutionDepthExceeded,
                "command substitution nesting exceeds the 32-level limit",
                SourceSpan::new(position, position + 2),
            ));
        }
        frames.push(CommandSubstitutionScanFrame::new());
        Ok(())
    }

    fn parse_parameter(&mut self, quote: QuoteMode) -> Result<Option<WordPart>, Diagnostic> {
        let parameter_start = self.position;
        debug_assert_eq!(self.bump(), Some('$'));
        let parameter = match self.peek() {
            Some('?') => {
                self.bump();
                Parameter::LastStatus
            }
            Some('{') => {
                self.bump();
                let name_start = self.position;
                let Some(first) = self.peek() else {
                    return Err(Diagnostic::new(
                        DiagnosticCode::UnterminatedParameterExpansion,
                        "braced parameter expansion is missing its closing brace",
                        SourceSpan::new(parameter_start, self.source.len()),
                    ));
                };
                if !is_identifier_start(first) {
                    let end = self.position + first.len_utf8();
                    return Err(Diagnostic::new(
                        DiagnosticCode::InvalidParameterExpansion,
                        "braced parameters require an ASCII identifier",
                        SourceSpan::new(parameter_start, end),
                    ));
                }
                self.bump();
                while self.peek().is_some_and(is_identifier_continue) {
                    self.bump();
                }
                let name_end = self.position;
                match self.peek() {
                    Some('}') => {
                        self.bump();
                    }
                    None => {
                        return Err(Diagnostic::new(
                            DiagnosticCode::UnterminatedParameterExpansion,
                            "braced parameter expansion is missing its closing brace",
                            SourceSpan::new(parameter_start, self.source.len()),
                        ));
                    }
                    Some(character) => {
                        return Err(Diagnostic::new(
                            DiagnosticCode::InvalidParameterExpansion,
                            "braced parameters support only a plain ASCII identifier",
                            SourceSpan::new(parameter_start, self.position + character.len_utf8()),
                        ));
                    }
                }
                Parameter::Variable {
                    name: self.source[name_start..name_end].to_owned(),
                    braced: true,
                }
            }
            Some(character) if is_identifier_start(character) => {
                let name_start = self.position;
                self.bump();
                while self.peek().is_some_and(is_identifier_continue) {
                    self.bump();
                }
                Parameter::Variable {
                    name: self.source[name_start..self.position].to_owned(),
                    braced: false,
                }
            }
            Some(character) if is_unsupported_special_parameter(character) => {
                return Err(Diagnostic::new(
                    DiagnosticCode::UnsupportedSyntax,
                    "only named parameters and `$?` are supported",
                    SourceSpan::new(parameter_start, self.position + character.len_utf8()),
                ));
            }
            _ => return Ok(None),
        };
        Ok(Some(WordPart::Parameter {
            parameter,
            quote,
            span: SourceSpan::new(parameter_start, self.position),
        }))
    }

    fn skip_command_layout(&mut self) {
        loop {
            self.skip_horizontal();
            match self.peek() {
                Some('\n') => {
                    self.bump();
                }
                Some('#') => {
                    self.skip_comment();
                    if self.peek() == Some('\n') {
                        self.bump();
                    }
                }
                _ => break,
            }
        }
    }

    fn skip_conditional_layout(&mut self) {
        loop {
            self.skip_horizontal();
            if self.peek() == Some('#') {
                self.skip_comment();
            }
            if self.peek() == Some('\n') {
                self.bump();
                continue;
            }
            break;
        }
        self.skip_horizontal();
    }

    fn skip_horizontal(&mut self) -> bool {
        let start = self.position;
        while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
            self.bump();
        }
        self.position != start
    }

    fn skip_comment(&mut self) {
        while !matches!(self.peek(), None | Some('\n')) {
            self.bump();
        }
    }

    fn diagnostic_here(&self, code: DiagnosticCode, message: &'static str) -> Diagnostic {
        let end = self.peek().map_or(self.position, |character| {
            self.position + character.len_utf8()
        });
        Diagnostic::new(code, message, SourceSpan::new(self.position, end))
    }

    fn conditional_separator_diagnostic(&self, message: &'static str) -> Diagnostic {
        Diagnostic::new(
            DiagnosticCode::UnexpectedSeparator,
            message,
            SourceSpan::new(self.position, self.position + 2),
        )
    }

    fn starts_conditional_operator(&self) -> bool {
        self.source[self.position..].starts_with("&&")
            || self.source[self.position..].starts_with("||")
    }

    fn starts_command_substitution(&self) -> bool {
        self.source[self.position..].starts_with("$(")
    }

    fn peek(&self) -> Option<char> {
        self.source.get(self.position..)?.chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.position += character.len_utf8();
        Some(character)
    }

    const fn is_eof(&self) -> bool {
        self.position == self.source.len()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandSubstitutionScanQuote {
    Unquoted,
    Single,
    Double,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CommandSubstitutionScanFrame {
    quote: CommandSubstitutionScanQuote,
    comment: bool,
    comment_boundary: bool,
}

impl CommandSubstitutionScanFrame {
    const fn new() -> Self {
        Self {
            quote: CommandSubstitutionScanQuote::Unquoted,
            comment: false,
            comment_boundary: true,
        }
    }
}

fn push_unquoted(
    parts: &mut Vec<WordPart>,
    value: &mut String,
    start: &mut Option<usize>,
    end: usize,
) {
    if let Some(start) = start.take() {
        parts.push(WordPart::Literal {
            value: std::mem::take(value),
            quote: QuoteMode::Unquoted,
            span: SourceSpan::new(start, end),
        });
    }
}

const fn is_reserved_operator(character: char) -> bool {
    matches!(character, '&' | '(' | ')' | '`')
}

const fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_ascii_alphabetic()
}

const fn is_identifier_continue(character: char) -> bool {
    character == '_' || character.is_ascii_alphanumeric()
}

const fn is_unsupported_special_parameter(character: char) -> bool {
    character.is_ascii_digit() || matches!(character, '#' | '@' | '*' | '-' | '!' | '$')
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::{
        ConditionalOperator, DiagnosticCode, Parameter, QuoteMode, RedirectionDescriptor,
        RedirectionFileMode, RedirectionTarget, SourceSpan,
    };

    #[test]
    fn parses_comments_separators_and_mixed_quotes() {
        let script = parse("# heading\necho pre\"mid\"'post'\\ value; pwd\n").expect("parse");
        assert_eq!(script.commands().len(), 2);
        assert_eq!(script.pipelines().len(), 2);
        let first = &script.commands()[0];
        assert_eq!(first.words().len(), 2);
        assert_eq!(first.words()[0].literal(), "echo");
        assert_eq!(first.words()[1].literal(), "premidpost value");
        assert_eq!(first.words()[1].parts().len(), 4);
        assert_eq!(first.words()[1].parts()[0].quote(), QuoteMode::Unquoted);
        assert_eq!(first.words()[1].parts()[1].quote(), QuoteMode::Double);
        assert_eq!(first.words()[1].parts()[2].quote(), QuoteMode::Single);
        assert_eq!(first.words()[1].parts()[3].quote(), QuoteMode::Unquoted);
        assert_eq!(script.commands()[1].words()[0].literal(), "pwd");
    }

    #[test]
    fn preserves_empty_quoted_arguments() {
        let script = parse("echo '' \"\"").expect("parse");
        let words = script.commands()[0].words();
        assert_eq!(words.len(), 3);
        assert_eq!(words[1].literal(), "");
        assert_eq!(words[2].literal(), "");
        assert_eq!(words[1].parts()[0].quote(), QuoteMode::Single);
        assert_eq!(words[2].parts()[0].quote(), QuoteMode::Double);
    }

    #[test]
    fn parses_named_and_status_parameters_with_exact_quote_and_source_spans() {
        let source = "echo $NAME \"${NAME}:$?\" '$NAME' \\$NAME $";
        let script = parse(source).expect("parse parameters");
        let words = script.commands()[0].words();

        assert_eq!(words[1].literal(), "$NAME");
        assert_eq!(words[1].parts()[0].span(), SourceSpan::new(5, 10));
        assert_eq!(words[1].parts()[0].quote(), QuoteMode::Unquoted);
        assert_eq!(
            words[1].parts()[0].parameter(),
            Some(&Parameter::Variable {
                name: "NAME".to_owned(),
                braced: false,
            })
        );

        assert_eq!(words[2].literal(), "${NAME}:$?");
        assert_eq!(words[2].parts().len(), 3);
        assert_eq!(words[2].parts()[0].span(), SourceSpan::new(12, 19));
        assert_eq!(words[2].parts()[0].quote(), QuoteMode::Double);
        assert_eq!(
            words[2].parts()[0].parameter(),
            Some(&Parameter::Variable {
                name: "NAME".to_owned(),
                braced: true,
            })
        );
        assert_eq!(words[2].parts()[1].value(), ":");
        assert_eq!(words[2].parts()[1].span(), SourceSpan::new(19, 20));
        assert_eq!(
            words[2].parts()[2].parameter(),
            Some(&Parameter::LastStatus)
        );
        assert_eq!(words[2].parts()[2].span(), SourceSpan::new(20, 22));

        assert_eq!(words[3].literal(), "$NAME");
        assert_eq!(words[3].parts()[0].quote(), QuoteMode::Single);
        assert!(words[3].parts()[0].parameter().is_none());
        assert_eq!(words[4].literal(), "$NAME");
        assert!(words[4].parts()[0].parameter().is_none());
        assert_eq!(words[5].literal(), "$");
        assert!(words[5].parts()[0].parameter().is_none());
    }

    #[test]
    fn parses_nested_command_substitutions_with_quote_and_outer_source_spans() {
        let source = r#"echo pre$(echo one; echo "$(echo two)")post "$(echo '#)'; echo \))" '$(echo no)' \$\("#;
        let script = parse(source).expect("parse command substitutions");
        let words = script.commands()[0].words();

        assert_eq!(words[1].parts().len(), 3);
        let substitution = words[1].parts()[1]
            .command_substitution()
            .expect("unquoted command substitution");
        assert_eq!(
            substitution.script().source(),
            r#"echo one; echo "$(echo two)""#
        );
        assert_eq!(substitution.script().commands().len(), 2);
        assert_eq!(
            substitution.span().source_text(source),
            Some(r#"$(echo one; echo "$(echo two)")"#)
        );
        assert_eq!(
            substitution.body_span().source_text(source),
            Some(r#"echo one; echo "$(echo two)""#)
        );
        let nested = substitution.script().commands()[1].words()[1].parts()[0]
            .command_substitution()
            .expect("nested command substitution");
        assert_eq!(nested.script().source(), "echo two");
        assert_eq!(words[1].parts()[1].quote(), QuoteMode::Unquoted);

        let quoted = words[2].parts()[0]
            .command_substitution()
            .expect("double-quoted command substitution");
        assert_eq!(quoted.script().source(), r#"echo '#)'; echo \)"#);
        assert_eq!(words[2].parts()[0].quote(), QuoteMode::Double);
        assert_eq!(words[3].literal(), "$(echo no)");
        assert!(
            words[3]
                .parts()
                .iter()
                .all(|part| part.command_substitution().is_none())
        );
        assert_eq!(words[4].literal(), "$(");
        assert_eq!(
            words[1].literal(),
            r#"pre$(echo one; echo "$(echo two)")post"#
        );
    }

    #[test]
    fn rejects_empty_unterminated_invalid_and_overdeep_command_substitutions() {
        for (source, code, span) in [
            (
                "echo $(",
                DiagnosticCode::UnterminatedCommandSubstitution,
                SourceSpan::new(5, 7),
            ),
            (
                "echo $()",
                DiagnosticCode::UnexpectedSeparator,
                SourceSpan::new(5, 7),
            ),
            (
                "echo $(# comment only\n)",
                DiagnosticCode::UnexpectedSeparator,
                SourceSpan::new(5, 7),
            ),
            (
                "echo $(echo |)",
                DiagnosticCode::UnexpectedSeparator,
                SourceSpan::new(12, 13),
            ),
        ] {
            let error = parse(source).expect_err("invalid command substitution");
            assert_eq!(error.code(), code, "source={source}");
            assert_eq!(error.span(), span, "source={source}");
        }

        let accepted = format!("echo {}echo deep{}", "$(".repeat(32), ")".repeat(32));
        parse(&accepted).expect("32 nested command substitutions");

        let source = format!("echo {}echo deep{}", "$(".repeat(33), ")".repeat(33));
        let error = parse(&source).expect_err("overdeep command substitution");
        assert_eq!(
            error.code(),
            DiagnosticCode::CommandSubstitutionDepthExceeded
        );
        assert_eq!(error.span().source_text(&source), Some("$("));
    }

    #[test]
    fn keeps_non_parameter_dollars_literal_in_unquoted_and_double_quoted_words() {
        let script = parse("echo $ $: \"$:\" \"\\$NAME\"").expect("literal dollars");
        let values: Vec<String> = script.commands()[0]
            .words()
            .iter()
            .map(crate::Word::literal)
            .collect();
        assert_eq!(values, ["echo", "$", "$:", "$:", "$NAME"]);
        assert!(
            script.commands()[0]
                .words()
                .iter()
                .flat_map(|word| word.parts())
                .all(|part| part.parameter().is_none())
        );
    }

    #[test]
    fn rejects_malformed_and_unsupported_parameter_forms_at_exact_spans() {
        for (source, code, span) in [
            (
                "${}",
                DiagnosticCode::InvalidParameterExpansion,
                SourceSpan::new(0, 3),
            ),
            (
                "${9}",
                DiagnosticCode::InvalidParameterExpansion,
                SourceSpan::new(0, 3),
            ),
            (
                "${NAME",
                DiagnosticCode::UnterminatedParameterExpansion,
                SourceSpan::new(0, 6),
            ),
            (
                "${NAME:-fallback}",
                DiagnosticCode::InvalidParameterExpansion,
                SourceSpan::new(0, 7),
            ),
            (
                "$1",
                DiagnosticCode::UnsupportedSyntax,
                SourceSpan::new(0, 2),
            ),
            (
                "$$",
                DiagnosticCode::UnsupportedSyntax,
                SourceSpan::new(0, 2),
            ),
            (
                "$((1 + 2))",
                DiagnosticCode::InvalidParameterExpansion,
                SourceSpan::new(0, 3),
            ),
        ] {
            let error = parse(source).expect_err("unsupported parameter form");
            assert_eq!(error.code(), code, "source={source}");
            assert_eq!(error.span(), span, "source={source}");
        }
    }

    #[test]
    fn parses_pipeline_stages_and_operator_spans_without_splitting_quotes() {
        let script = parse("echo 'left|literal'|grep nope").expect("parse pipeline");
        assert_eq!(script.commands().len(), 2);
        assert_eq!(script.commands()[0].words()[1].literal(), "left|literal");
        assert_eq!(script.commands()[1].words()[0].literal(), "grep");
        assert_eq!(script.pipelines().len(), 1);
        assert_eq!(script.pipelines()[0].command_range(), 0..2);
        assert_eq!(
            script.pipelines()[0].pipe_spans(),
            [SourceSpan::new(19, 20)]
        );
        assert_eq!(script.pipelines()[0].span(), SourceSpan::new(0, 29));
    }

    #[test]
    fn parses_left_associative_conditional_lists_with_continuation_layout() {
        let source = "echo first>out&& # continue the list\n echo second||echo third; echo fourth";
        let script = parse(source).expect("parse conditional list");

        assert_eq!(script.commands().len(), 4);
        assert_eq!(script.pipelines().len(), 4);
        assert_eq!(script.pipelines()[0].condition(), None);
        let RedirectionTarget::File { path, .. } = script.commands()[0].redirections()[0].target()
        else {
            panic!("first pipeline retains its adjacent file redirection");
        };
        assert_eq!(path.literal(), "out");
        assert_eq!(
            script.pipelines()[1]
                .condition()
                .expect("AND condition")
                .operator(),
            ConditionalOperator::AndIf
        );
        assert_eq!(
            script.pipelines()[1]
                .condition()
                .expect("AND condition")
                .span()
                .source_text(source),
            Some("&&")
        );
        assert_eq!(
            script.pipelines()[2]
                .condition()
                .expect("OR condition")
                .operator(),
            ConditionalOperator::OrIf
        );
        assert_eq!(
            script.pipelines()[2]
                .condition()
                .expect("OR condition")
                .span()
                .source_text(source),
            Some("||")
        );
        assert_eq!(script.pipelines()[3].condition(), None);
        assert_eq!(
            script.pipelines()[1].span().source_text(source),
            Some("echo second")
        );

        let adjacent = parse("echo&&next; echo 2>&1||next").expect("parse adjacent operators");
        assert_eq!(adjacent.pipelines().len(), 4);
        assert_eq!(
            adjacent.pipelines()[1]
                .condition()
                .expect("adjacent AND condition")
                .operator(),
            ConditionalOperator::AndIf
        );
        assert_eq!(adjacent.pipelines()[2].condition(), None);
        assert_eq!(
            adjacent.pipelines()[3]
                .condition()
                .expect("descriptor-adjacent OR condition")
                .operator(),
            ConditionalOperator::OrIf
        );
        assert_eq!(
            adjacent.commands()[2].redirections()[0].target(),
            &RedirectionTarget::Descriptor(RedirectionDescriptor::Stdout)
        );

        let literal = parse(r#"echo '&&' "||" \&\&"#).expect("parse literal operator bytes");
        assert_eq!(literal.pipelines().len(), 1);
        assert_eq!(
            literal.commands()[0]
                .words()
                .iter()
                .map(crate::Word::literal)
                .collect::<Vec<_>>(),
            ["echo", "&&", "||", "&&"]
        );
    }

    #[test]
    fn parses_source_ordered_file_and_descriptor_redirections() {
        let source = "tool<input >out 2>>errors 2>&1 1>&2";
        let script = parse(source).expect("parse redirections");
        let command = &script.commands()[0];
        assert_eq!(command.words().len(), 1);
        assert_eq!(command.words()[0].literal(), "tool");
        assert_eq!(command.redirections().len(), 5);

        let expected = [
            (
                RedirectionDescriptor::Stdin,
                RedirectionFileMode::Read,
                "input",
                "<",
            ),
            (
                RedirectionDescriptor::Stdout,
                RedirectionFileMode::Write,
                "out",
                ">",
            ),
            (
                RedirectionDescriptor::Stderr,
                RedirectionFileMode::Append,
                "errors",
                "2>>",
            ),
        ];
        for (redirection, (descriptor, mode, path, operator)) in
            command.redirections()[..3].iter().zip(expected)
        {
            assert_eq!(redirection.descriptor(), descriptor);
            let RedirectionTarget::File {
                path: actual_path,
                mode: actual_mode,
            } = redirection.target()
            else {
                panic!("expected file redirection");
            };
            assert_eq!(*actual_mode, mode);
            assert_eq!(actual_path.literal(), path);
            assert_eq!(
                redirection.operator_span().source_text(source),
                Some(operator)
            );
        }
        assert_eq!(
            command.redirections()[3].target(),
            &RedirectionTarget::Descriptor(RedirectionDescriptor::Stdout)
        );
        assert_eq!(
            command.redirections()[3]
                .operator_span()
                .source_text(source),
            Some("2>&1")
        );
        assert_eq!(
            command.redirections()[4].target(),
            &RedirectionTarget::Descriptor(RedirectionDescriptor::Stderr)
        );
        assert_eq!(
            command.redirections()[4]
                .operator_span()
                .source_text(source),
            Some("1>&2")
        );
        assert_eq!(command.span().source_text(source), Some(source));
    }

    #[test]
    fn rejects_invalid_or_unfinished_redirections_at_the_operator() {
        for (source, code, text) in [
            (
                "tool >",
                DiagnosticCode::UnexpectedSeparator,
                "a file redirection operator must be followed by a target",
            ),
            (
                "tool > >out",
                DiagnosticCode::UnexpectedSeparator,
                "a file redirection operator must be followed by a target",
            ),
            (
                "tool 3>out",
                DiagnosticCode::InvalidRedirection,
                "output redirection supports only descriptors 1 and 2",
            ),
            (
                "tool 2>&3",
                DiagnosticCode::InvalidRedirection,
                "descriptor duplication supports only descriptors 1 and 2",
            ),
            (
                "tool 2>&1#suffix",
                DiagnosticCode::InvalidRedirection,
                "a duplicated descriptor must end at an operator or word boundary",
            ),
            (
                "tool >>&1",
                DiagnosticCode::InvalidRedirection,
                "append redirection cannot duplicate a descriptor",
            ),
            (
                "tool <<EOF",
                DiagnosticCode::UnsupportedSyntax,
                "here documents are reserved for a later milestone",
            ),
        ] {
            let error = parse(source).expect_err("invalid redirection");
            assert_eq!(error.code(), code, "source={source}");
            assert_eq!(error.message(), text, "source={source}");
        }
    }

    #[test]
    fn rejects_missing_pipeline_and_conditional_operands() {
        for (source, code, span) in [
            (
                "| echo",
                DiagnosticCode::UnexpectedSeparator,
                SourceSpan::new(0, 1),
            ),
            (
                "echo |",
                DiagnosticCode::UnexpectedSeparator,
                SourceSpan::new(5, 6),
            ),
            (
                "echo | | grep",
                DiagnosticCode::UnexpectedSeparator,
                SourceSpan::new(5, 6),
            ),
            (
                "&& echo",
                DiagnosticCode::UnexpectedSeparator,
                SourceSpan::new(0, 2),
            ),
            (
                "echo &&",
                DiagnosticCode::UnexpectedSeparator,
                SourceSpan::new(5, 7),
            ),
            (
                "echo || ; grep",
                DiagnosticCode::UnexpectedSeparator,
                SourceSpan::new(5, 7),
            ),
            (
                "echo && || grep",
                DiagnosticCode::UnexpectedSeparator,
                SourceSpan::new(5, 7),
            ),
            (
                "echo ||| grep",
                DiagnosticCode::UnexpectedSeparator,
                SourceSpan::new(5, 7),
            ),
            (
                "echo & grep",
                DiagnosticCode::UnsupportedSyntax,
                SourceSpan::new(5, 6),
            ),
        ] {
            let error = parse(source).expect_err("invalid pipeline");
            assert_eq!(error.code(), code, "source={source}");
            assert_eq!(error.span(), span, "source={source}");
        }
    }

    #[test]
    fn rejects_unterminated_quotes_at_the_opening_quote() {
        let error = parse("echo \"unfinished").expect_err("missing quote");
        assert_eq!(error.code(), DiagnosticCode::UnterminatedDoubleQuote);
        assert_eq!(error.span(), SourceSpan::new(5, 16));
    }

    #[test]
    fn rejects_repeated_semicolons() {
        let error = parse("echo ok;;pwd").expect_err("repeated separator");
        assert_eq!(error.code(), DiagnosticCode::UnexpectedSeparator);
        assert_eq!(error.span(), SourceSpan::new(8, 9));
    }

    #[test]
    fn spans_are_utf8_byte_ranges_into_the_original_source() {
        let source = "echo '你好'\n";
        let script = parse(source).expect("parse UTF-8");
        let argument = &script.commands()[0].words()[1];
        assert_eq!(argument.span(), SourceSpan::new(5, 13));
        assert_eq!(argument.span().source_text(source), Some("'你好'"));
        assert_eq!(argument.literal(), "你好");
    }
}
