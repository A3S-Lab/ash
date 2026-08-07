use crate::{
    Diagnostic, DiagnosticCode, Parameter, Pipeline, QuoteMode, Script, SimpleCommand, SourceSpan,
    Word, WordPart,
};

/// Parses the H0 shell syntax subset.
///
/// The current subset accepts foreground native-pipeline syntax with `|`, simple
/// commands separated by a newline or `;`, shell comments, single and double
/// quotes, backslash escaping, named parameters, and `$?`. Syntax reserved for
/// later milestones is rejected with a source span instead of being
/// reinterpreted as a literal argument.
pub fn parse(source: &str) -> Result<Script, Diagnostic> {
    Parser::new(source).parse()
}

struct Parser<'a> {
    source: &'a str,
    position: usize,
}

impl<'a> Parser<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            source,
            position: 0,
        }
    }

    fn parse(mut self) -> Result<Script, Diagnostic> {
        let mut commands = Vec::new();
        let mut pipelines = Vec::new();
        self.skip_command_layout();

        while !self.is_eof() {
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
                loop {
                    let separated = self.skip_horizontal();
                    match self.peek() {
                        None | Some('\n' | ';' | '|') => break,
                        Some('#') if separated => break,
                        Some(_) => words.push(self.parse_word()?),
                    }
                }

                let Some(command_end) = words.last().map(|last| last.span().end()) else {
                    return Err(self.diagnostic_here(
                        DiagnosticCode::UnexpectedSeparator,
                        "a pipeline stage must contain a command",
                    ));
                };
                commands.push(SimpleCommand::new(
                    words,
                    SourceSpan::new(command_start, command_end),
                ));

                self.skip_horizontal();
                if self.peek() == Some('#') {
                    self.skip_comment();
                    pipeline_end = command_end;
                    break;
                }
                match self.peek() {
                    Some('|') => {
                        let pipe_start = self.position;
                        self.bump();
                        if self.peek() == Some('|') {
                            return Err(Diagnostic::new(
                                DiagnosticCode::UnsupportedSyntax,
                                "conditional OR lists are reserved for a later milestone",
                                SourceSpan::new(pipe_start, self.position + 1),
                            ));
                        }
                        let pipe_span = SourceSpan::new(pipe_start, self.position);
                        pipe_spans.push(pipe_span);
                        self.skip_horizontal();
                        if matches!(self.peek(), None | Some('\n' | ';' | '|' | '#')) {
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
                SourceSpan::new(pipeline_start, pipeline_end),
            ));

            match self.peek() {
                None => break,
                Some('\n' | ';') => {
                    self.bump();
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

    fn parse_word(&mut self) -> Result<Word, Diagnostic> {
        let word_start = self.position;
        let mut parts = Vec::new();
        let mut unquoted = String::new();
        let mut unquoted_start = None;

        while let Some(character) = self.peek() {
            if matches!(character, ' ' | '\t' | '\r' | '\n' | ';') {
                break;
            }
            if character == '|' {
                break;
            }
            if is_reserved_operator(character) {
                return Err(self.diagnostic_here(
                    DiagnosticCode::UnsupportedSyntax,
                    "redirections, background jobs, and subshells are reserved for a later milestone",
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
                    let dollar_start = self.position;
                    if let Some(parameter) = self.parse_parameter(QuoteMode::Unquoted)? {
                        parts.push(parameter);
                    } else {
                        unquoted_start = Some(dollar_start);
                        unquoted.push('$');
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
                    if let Some(parameter) = self.parse_parameter(QuoteMode::Double)? {
                        if !value.is_empty() {
                            parts.push(WordPart::Literal {
                                value: std::mem::take(&mut value),
                                quote: QuoteMode::Double,
                                span: SourceSpan::new(literal_start, dollar_start),
                            });
                        }
                        parts.push(parameter);
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
            Some('(') => {
                return Err(Diagnostic::new(
                    DiagnosticCode::UnsupportedSyntax,
                    "command substitution is reserved for a later milestone",
                    SourceSpan::new(parameter_start, self.position + 1),
                ));
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
    matches!(character, '&' | '<' | '>' | '(' | ')' | '`')
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
    use crate::{DiagnosticCode, Parameter, QuoteMode, SourceSpan};

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
                "$(echo)",
                DiagnosticCode::UnsupportedSyntax,
                SourceSpan::new(0, 2),
            ),
            (
                "$$",
                DiagnosticCode::UnsupportedSyntax,
                SourceSpan::new(0, 2),
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
    fn rejects_missing_pipeline_stages_and_reserved_conditional_or() {
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
                "echo || grep",
                DiagnosticCode::UnsupportedSyntax,
                SourceSpan::new(5, 7),
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
