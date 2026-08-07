use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use a3s_ash_shell::{
    CommandResolver, Diagnostic, HostPlatform, NativeCommandLookup, PlatformEnvironment,
    PortableCommand, QuoteMode, ResolutionError, ResolvedCommand, Script, ShellFunction,
    ShellState, StatefulBuiltin, parse,
};

#[test]
fn parser_ast_fixture_is_stable() {
    let source = include_str!("fixtures/parser/basic.ash");
    let expected = include_str!("fixtures/parser/basic.ast");
    let script = parse(source).expect("parse golden source");
    assert_eq!(render_script(&script), expected);
}

#[test]
fn parser_diagnostic_fixture_is_stable() {
    let source = include_str!("fixtures/parser/unsupported-pipeline.ash");
    let expected = include_str!("fixtures/parser/unsupported-pipeline.diagnostic");
    let diagnostic = parse(source).expect_err("pipeline is not in H0");
    assert_eq!(render_diagnostic(&diagnostic), expected);
}

#[test]
fn command_resolution_fixture_is_stable() {
    let mut state = ShellState::new("/fixture");
    state.set_alias("ll", "ls -la").expect("alias");
    state
        .set_function(
            "deploy",
            ShellFunction::new(parse("echo deploy").expect("function body")),
        )
        .expect("function");
    state
        .options_mut()
        .set_wsl_distribution(Some("Ubuntu".to_owned()));
    let resolver = CommandResolver::for_platform(&state, FixtureLookup, HostPlatform::Windows);

    for (line_number, line) in include_str!("fixtures/resolution/basic.tsv")
        .lines()
        .enumerate()
    {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (command, expected) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("invalid fixture row {}", line_number + 1));
        assert_eq!(
            render_resolution(resolver.resolve(command)),
            expected,
            "fixture row {}",
            line_number + 1
        );
    }
}

fn render_script(script: &Script) -> String {
    let mut output = String::new();
    writeln!(
        output,
        "script {}..{}",
        script.span().start(),
        script.span().end()
    )
    .expect("string write");
    for command in script.commands() {
        writeln!(
            output,
            "command {}..{}",
            command.span().start(),
            command.span().end()
        )
        .expect("string write");
        for word in command.words() {
            writeln!(
                output,
                "  word {}..{}",
                word.span().start(),
                word.span().end()
            )
            .expect("string write");
            for part in word.parts() {
                writeln!(
                    output,
                    "    literal {} {}..{} \"{}\"",
                    quote_name(part.quote()),
                    part.span().start(),
                    part.span().end(),
                    part.value().escape_debug()
                )
                .expect("string write");
            }
        }
    }
    output
}

const fn quote_name(quote: QuoteMode) -> &'static str {
    match quote {
        QuoteMode::Unquoted => "unquoted",
        QuoteMode::Single => "single",
        QuoteMode::Double => "double",
    }
}

fn render_diagnostic(diagnostic: &Diagnostic) -> String {
    format!(
        "{:?}\t{}..{}\t{}\n",
        diagnostic.code(),
        diagnostic.span().start(),
        diagnostic.span().end(),
        diagnostic.message()
    )
}

#[derive(Clone, Copy)]
struct FixtureLookup;

impl NativeCommandLookup for FixtureLookup {
    fn resolve(
        &self,
        command: &str,
        _cwd: &Path,
        _environment: &PlatformEnvironment,
    ) -> Option<PathBuf> {
        (command == "cargo").then(|| PathBuf::from("/fixture/bin/cargo"))
    }
}

fn render_resolution(result: Result<ResolvedCommand, ResolutionError>) -> String {
    match result {
        Ok(ResolvedCommand::StatefulBuiltin(command)) => {
            format!("builtin:{}", stateful_name(command))
        }
        Ok(ResolvedCommand::Alias { name, replacement }) => {
            format!("alias:{name}:{replacement}")
        }
        Ok(ResolvedCommand::Function { name }) => format!("function:{name}"),
        Ok(ResolvedCommand::Portable(command)) => {
            format!("portable:{}", portable_name(command))
        }
        Ok(ResolvedCommand::Native {
            executable,
            explicit,
        }) => format!(
            "native:{}:{}",
            executable.to_string_lossy().replace('\\', "/"),
            if explicit { "explicit" } else { "implicit" }
        ),
        Ok(ResolvedCommand::Wsl {
            command,
            distribution,
        }) => format!(
            "wsl:{}:{command}",
            distribution.as_deref().unwrap_or("default")
        ),
        Err(ResolutionError::EmptyCommand) => "error:empty".to_owned(),
        Err(ResolutionError::CommandNotFound { command }) => {
            format!("error:not-found:{command}")
        }
        Err(ResolutionError::BackendUnavailable { backend }) => {
            format!("error:backend-unavailable:{backend}")
        }
        #[allow(unreachable_patterns)]
        Ok(_) | Err(_) => "error:unknown-contract-variant".to_owned(),
    }
}

const fn stateful_name(command: StatefulBuiltin) -> &'static str {
    command.name()
}

const fn portable_name(command: PortableCommand) -> &'static str {
    command.name()
}
