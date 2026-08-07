use crate::{
    Diagnostic, DiagnosticCode, QuoteMode, Script, SimpleCommand, SourceSpan, Word, WordPart,
};

/// Parses the H0 shell syntax subset.
///
/// H0 accepts foreground simple commands separated by a newline or `;`, shell
/// comments, single and double quotes, and backslash escaping. Syntax reserved
/// for later milestones is rejected with a source span instead of being
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
        self.skip_command_layout();

        while !self.is_eof() {
            if self.peek() == Some(';') {
                return Err(self.diagnostic_here(
                    DiagnosticCode::UnexpectedSeparator,
                    "a command separator must follow a command",
                ));
            }

            let command_start = self.position;
            let mut words = Vec::new();
            loop {
                let separated = self.skip_horizontal();
                match self.peek() {
                    None | Some('\n' | ';') => break,
                    Some('#') if separated => {
                        self.skip_comment();
                        break;
                    }
                    Some(_) => words.push(self.parse_word()?),
                }
            }

            if let Some(command_end) = words.last().map(|last| last.span().end()) {
                commands.push(SimpleCommand::new(
                    words,
                    SourceSpan::new(command_start, command_end),
                ));
            }

            self.skip_horizontal();
            if self.peek() == Some('#') {
                self.skip_comment();
            }
            match self.peek() {
                None => break,
                Some('\n' | ';') => {
                    self.bump();
                    self.skip_command_layout();
                }
                Some(_) => {
                    return Err(self.diagnostic_here(
                        DiagnosticCode::UnsupportedSyntax,
                        "unsupported syntax follows a simple command",
                    ));
                }
            }
        }

        Ok(Script::new(self.source.to_owned(), commands))
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
            if is_reserved_operator(character) {
                return Err(self.diagnostic_here(
                    DiagnosticCode::UnsupportedSyntax,
                    "pipelines, redirections, background jobs, and subshells are reserved for a later milestone",
                ));
            }
            if character == '$' {
                return Err(self.diagnostic_here(
                    DiagnosticCode::UnsupportedSyntax,
                    "parameter and command substitution are reserved for a later milestone",
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
                    parts.push(self.parse_double_quoted()?);
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

    fn parse_double_quoted(&mut self) -> Result<WordPart, Diagnostic> {
        let quote_start = self.position;
        self.bump();
        let mut value = String::new();
        loop {
            match self.peek() {
                Some('"') => {
                    self.bump();
                    return Ok(WordPart::Literal {
                        value,
                        quote: QuoteMode::Double,
                        span: SourceSpan::new(quote_start, self.position),
                    });
                }
                Some('$') => {
                    return Err(self.diagnostic_here(
                        DiagnosticCode::UnsupportedSyntax,
                        "parameter and command substitution are reserved for a later milestone",
                    ));
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
    matches!(character, '|' | '&' | '<' | '>' | '(' | ')' | '`')
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::{DiagnosticCode, QuoteMode, SourceSpan};

    #[test]
    fn parses_comments_separators_and_mixed_quotes() {
        let script = parse("# heading\necho pre\"mid\"'post'\\ value; pwd\n").expect("parse");
        assert_eq!(script.commands().len(), 2);
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
    fn rejects_reserved_syntax_at_the_operator_span() {
        let error = parse("echo ok | grep nope").expect_err("pipeline is H2");
        assert_eq!(error.code(), DiagnosticCode::UnsupportedSyntax);
        assert_eq!(error.span(), SourceSpan::new(8, 9));
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
