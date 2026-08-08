use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::SourceSpan;
use crate::expand::{ExpandedWord, PathnameSegment};

/// Maximum intermediate candidates or final matches produced by one command.
pub const MAX_PATHNAME_EXPANSION_MATCHES: usize = 4_096;

/// Maximum directory entries inspected while expanding one command.
pub const MAX_PATHNAME_EXPANSION_ENTRIES: usize = 65_536;

/// Maximum native code units retained across active patterns in one command.
pub const MAX_PATHNAME_PATTERN_UNITS: usize = 32_768;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathnameExpansionErrorKind {
    InvalidPattern,
    NoMatches,
    ResourceLimit,
    Filesystem,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PathnameExpansionError {
    kind: PathnameExpansionErrorKind,
    message: String,
    span: SourceSpan,
}

impl PathnameExpansionError {
    pub(crate) const fn kind(&self) -> PathnameExpansionErrorKind {
        self.kind
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) const fn span(&self) -> SourceSpan {
        self.span
    }

    fn invalid(message: impl Into<String>, span: SourceSpan) -> Self {
        Self {
            kind: PathnameExpansionErrorKind::InvalidPattern,
            message: message.into(),
            span,
        }
    }

    fn no_matches(span: SourceSpan) -> Self {
        Self {
            kind: PathnameExpansionErrorKind::NoMatches,
            message: "pathname pattern matched no paths".to_owned(),
            span,
        }
    }

    fn resource(message: impl Into<String>, span: SourceSpan) -> Self {
        Self {
            kind: PathnameExpansionErrorKind::ResourceLimit,
            message: message.into(),
            span,
        }
    }

    fn filesystem(error: &io::Error, span: SourceSpan) -> Self {
        Self {
            kind: PathnameExpansionErrorKind::Filesystem,
            message: format!("pathname expansion could not read the filesystem: {error}"),
            span,
        }
    }
}

#[cfg(unix)]
type NativeUnit = u8;
#[cfg(windows)]
type NativeUnit = u16;
#[cfg(not(any(unix, windows)))]
type NativeUnit = char;

#[derive(Clone, Debug, Eq, PartialEq)]
enum PatternToken {
    Literal(NativeUnit),
    AnyOne,
    AnySequence,
    Class {
        negated: bool,
        ranges: Vec<(NativeUnit, NativeUnit)>,
    },
}

impl PatternToken {
    fn matches(&self, candidate: NativeUnit) -> bool {
        match self {
            Self::Literal(expected) => *expected == candidate,
            Self::AnyOne => true,
            Self::AnySequence => unreachable!("sequence matching is handled by the DP step"),
            Self::Class { negated, ranges } => {
                let contained = ranges
                    .iter()
                    .any(|(start, end)| *start <= candidate && candidate <= *end);
                contained != *negated
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PathPatternComponent {
    Prefix(OsString),
    Root(OsString),
    CurDir(OsString),
    ParentDir(OsString),
    Normal {
        value: OsString,
        pattern: Option<Vec<PatternToken>>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompiledPathPattern {
    components: Vec<PathPatternComponent>,
    has_pattern: bool,
    requires_directory: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GlobCandidate {
    lookup: PathBuf,
    argument: PathBuf,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PathnameExpansionBudget {
    pattern_units: usize,
    inspected_entries: usize,
    matches: usize,
}

#[cfg(test)]
pub(crate) fn expand_pathnames(
    words: Vec<ExpandedWord>,
    cwd: &Path,
) -> Result<Vec<ExpandedWord>, PathnameExpansionError> {
    expand_pathnames_with_budget(words, cwd, &mut PathnameExpansionBudget::default())
}

pub(crate) fn expand_pathnames_with_budget(
    words: Vec<ExpandedWord>,
    cwd: &Path,
    budget: &mut PathnameExpansionBudget,
) -> Result<Vec<ExpandedWord>, PathnameExpansionError> {
    let mut expanded = Vec::new();
    for word in words {
        expanded.extend(expand_pathname(word, cwd, budget)?);
    }
    Ok(expanded)
}

fn expand_pathname(
    word: ExpandedWord,
    cwd: &Path,
    budget: &mut PathnameExpansionBudget,
) -> Result<Vec<ExpandedWord>, PathnameExpansionError> {
    if !has_active_pattern_introducer(word.value(), word.pathname_segments()) {
        return Ok(vec![word]);
    }
    let compiled = compile_path_pattern(&word, budget)?;
    if !compiled.has_pattern {
        return Ok(vec![word]);
    }

    let span = word.span();
    let mut candidates = vec![GlobCandidate {
        lookup: if Path::new(word.value()).is_absolute() {
            PathBuf::new()
        } else {
            cwd.to_path_buf()
        },
        argument: PathBuf::new(),
    }];
    for component in compiled.components {
        match component {
            PathPatternComponent::Prefix(value)
            | PathPatternComponent::Root(value)
            | PathPatternComponent::CurDir(value)
            | PathPatternComponent::ParentDir(value) => {
                for candidate in &mut candidates {
                    candidate.lookup.push(&value);
                    candidate.argument.push(&value);
                }
            }
            PathPatternComponent::Normal {
                value,
                pattern: None,
            } => {
                for candidate in &mut candidates {
                    candidate.lookup.push(&value);
                    candidate.argument.push(&value);
                }
            }
            PathPatternComponent::Normal {
                pattern: Some(pattern),
                ..
            } => {
                let mut matches = Vec::new();
                for candidate in candidates {
                    let entries = match fs::read_dir(&candidate.lookup) {
                        Ok(entries) => entries,
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                            ) =>
                        {
                            continue;
                        }
                        Err(error) => return Err(PathnameExpansionError::filesystem(&error, span)),
                    };
                    for entry in entries {
                        budget.inspected_entries = budget.inspected_entries.saturating_add(1);
                        if budget.inspected_entries > MAX_PATHNAME_EXPANSION_ENTRIES {
                            return Err(PathnameExpansionError::resource(
                                format!(
                                    "pathname expansion inspected more than {MAX_PATHNAME_EXPANSION_ENTRIES} directory entries"
                                ),
                                span,
                            ));
                        }
                        let entry = entry
                            .map_err(|error| PathnameExpansionError::filesystem(&error, span))?;
                        let file_name = entry.file_name();
                        if !matches_component(&pattern, &native_units(&file_name)) {
                            continue;
                        }
                        let mut argument = candidate.argument.clone();
                        argument.push(&file_name);
                        budget.matches = budget.matches.saturating_add(1);
                        if budget.matches > MAX_PATHNAME_EXPANSION_MATCHES {
                            return Err(PathnameExpansionError::resource(
                                format!(
                                    "pathname expansion produced more than {MAX_PATHNAME_EXPANSION_MATCHES} intermediate or final matches"
                                ),
                                span,
                            ));
                        }
                        matches.push(GlobCandidate {
                            lookup: entry.path(),
                            argument,
                        });
                    }
                }
                candidates = matches;
            }
        }
    }

    let mut matches = Vec::new();
    for candidate in candidates {
        let metadata = match if compiled.requires_directory {
            fs::metadata(&candidate.lookup)
        } else {
            fs::symlink_metadata(&candidate.lookup)
        } {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(error) => return Err(PathnameExpansionError::filesystem(&error, span)),
        };
        if compiled.requires_directory && !metadata.file_type().is_dir() {
            continue;
        }
        matches.push(candidate.argument);
    }
    if matches.is_empty() {
        return Err(PathnameExpansionError::no_matches(span));
    }
    matches.sort_by(|left, right| compare_native(left.as_os_str(), right.as_os_str()));
    matches.dedup();
    Ok(matches
        .into_iter()
        .map(|path| ExpandedWord::from_pathname(path.into_os_string(), span))
        .collect())
}

fn compile_path_pattern(
    word: &ExpandedWord,
    budget: &mut PathnameExpansionBudget,
) -> Result<CompiledPathPattern, PathnameExpansionError> {
    let pattern_units = word
        .pathname_segments()
        .iter()
        .fold(0_usize, |total, segment| {
            total.saturating_add(native_unit_len(segment.value()))
        });
    budget.pattern_units = budget.pattern_units.saturating_add(pattern_units);
    if budget.pattern_units > MAX_PATHNAME_PATTERN_UNITS {
        return Err(PathnameExpansionError::resource(
            format!(
                "pathname expansion retained more than {MAX_PATHNAME_PATTERN_UNITS} active pattern units"
            ),
            word.span(),
        ));
    }
    let (units, active) = flatten_segments(word.pathname_segments());
    debug_assert_eq!(units.len(), active.len());
    debug_assert_eq!(units.len(), pattern_units);
    let mut cursor = 0_usize;
    let mut components = Vec::new();
    let mut has_pattern = false;

    for component in Path::new(word.value()).components() {
        let value = component.as_os_str().to_owned();
        let component_units = native_units(&value);
        let range =
            locate_component(&units, cursor, &component_units, &component).ok_or_else(|| {
                PathnameExpansionError::invalid(
                    "pathname pattern could not be mapped to native path components",
                    word.span(),
                )
            })?;
        cursor = range.end;
        let component_active = &active[range];
        let compiled = match component {
            Component::Prefix(_) => {
                if contains_active_prefix_operator(&component_units, component_active) {
                    return Err(PathnameExpansionError::invalid(
                        "pathname operators are not allowed in a platform path prefix",
                        word.span(),
                    ));
                }
                PathPatternComponent::Prefix(value)
            }
            Component::RootDir => PathPatternComponent::Root(value),
            Component::CurDir => PathPatternComponent::CurDir(value),
            Component::ParentDir => PathPatternComponent::ParentDir(value),
            Component::Normal(_) => {
                let pattern = compile_component(&component_units, component_active)
                    .map_err(|message| PathnameExpansionError::invalid(message, word.span()))?;
                has_pattern |= pattern.is_some();
                PathPatternComponent::Normal { value, pattern }
            }
        };
        components.push(compiled);
    }

    Ok(CompiledPathPattern {
        components,
        has_pattern,
        requires_directory: units.last().is_some_and(|unit| is_separator(*unit)),
    })
}

#[cfg(not(windows))]
fn has_active_pattern_introducer(_value: &OsStr, segments: &[PathnameSegment]) -> bool {
    segments
        .iter()
        .any(|segment| segment.is_active() && contains_pattern_introducer(segment.value()))
}

#[cfg(windows)]
fn has_active_pattern_introducer(value: &OsStr, segments: &[PathnameSegment]) -> bool {
    use std::os::windows::ffi::OsStrExt as _;

    let mut prefix = value.encode_wide();
    let verbatim_prefix = prefix.next() == Some(ascii('\\'))
        && prefix.next() == Some(ascii('\\'))
        && prefix.next() == Some(ascii('?'))
        && prefix.next() == Some(ascii('\\'));
    let mut offset = 0_usize;
    for segment in segments {
        for unit in segment.value().encode_wide() {
            let introducer = unit == ascii('*') || unit == ascii('?') || unit == ascii('[');
            if segment.is_active()
                && introducer
                && !(verbatim_prefix && offset == 2 && unit == ascii('?'))
            {
                return true;
            }
            offset = offset.saturating_add(1);
        }
    }
    false
}

fn flatten_segments(segments: &[PathnameSegment]) -> (Vec<NativeUnit>, Vec<bool>) {
    let mut units = Vec::new();
    let mut active = Vec::new();
    for segment in segments {
        let segment_units = native_units(segment.value());
        active.extend(std::iter::repeat_n(
            segment.is_active(),
            segment_units.len(),
        ));
        units.extend(segment_units);
    }
    (units, active)
}

fn locate_component(
    path: &[NativeUnit],
    cursor: usize,
    component: &[NativeUnit],
    kind: &Component<'_>,
) -> Option<std::ops::Range<usize>> {
    if matches!(kind, Component::RootDir) {
        let start = path[cursor..].iter().position(|unit| is_separator(*unit))? + cursor;
        return Some(start..start + 1);
    }
    let start = find_subslice(path, cursor, component)?;
    Some(start..start + component.len())
}

fn find_subslice(haystack: &[NativeUnit], start: usize, needle: &[NativeUnit]) -> Option<usize> {
    if needle.is_empty() {
        return Some(start);
    }
    haystack
        .get(start..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

fn contains_active_prefix_operator(units: &[NativeUnit], active: &[bool]) -> bool {
    units
        .iter()
        .zip(active)
        .enumerate()
        .any(|(index, (unit, active))| {
            *active && is_pattern_operator(*unit) && !is_intrinsic_prefix_operator(units, index)
        })
}

fn is_pattern_operator(unit: NativeUnit) -> bool {
    unit == ascii('*') || unit == ascii('?') || unit == ascii('[') || unit == ascii(']')
}

#[cfg(windows)]
fn is_intrinsic_prefix_operator(units: &[NativeUnit], index: usize) -> bool {
    index == 2
        && units.first() == Some(&ascii('\\'))
        && units.get(1) == Some(&ascii('\\'))
        && units.get(2) == Some(&ascii('?'))
        && units.get(3) == Some(&ascii('\\'))
}

#[cfg(not(windows))]
fn is_intrinsic_prefix_operator(_units: &[NativeUnit], _index: usize) -> bool {
    false
}

fn compile_component(
    units: &[NativeUnit],
    active: &[bool],
) -> Result<Option<Vec<PatternToken>>, String> {
    debug_assert_eq!(units.len(), active.len());
    let mut tokens = Vec::new();
    let mut has_pattern = false;
    let mut index = 0_usize;
    while index < units.len() {
        let unit = units[index];
        if active[index] && unit == ascii('*') {
            has_pattern = true;
            if !matches!(tokens.last(), Some(PatternToken::AnySequence)) {
                tokens.push(PatternToken::AnySequence);
            }
            index += 1;
        } else if active[index] && unit == ascii('?') {
            has_pattern = true;
            tokens.push(PatternToken::AnyOne);
            index += 1;
        } else if active[index] && unit == ascii('[') {
            let (class, next) = compile_class(units, active, index)?;
            has_pattern = true;
            tokens.push(class);
            index = next;
        } else {
            tokens.push(PatternToken::Literal(unit));
            index += 1;
        }
    }
    Ok(has_pattern.then_some(tokens))
}

fn compile_class(
    units: &[NativeUnit],
    active: &[bool],
    opening: usize,
) -> Result<(PatternToken, usize), String> {
    let mut content_start = opening + 1;
    let negated = content_start < units.len()
        && active[content_start]
        && matches!(units[content_start], value if value == ascii('!') || value == ascii('^'));
    if negated {
        content_start += 1;
    }
    let closing = (content_start..units.len())
        .find(|index| active[*index] && units[*index] == ascii(']'))
        .ok_or_else(|| "pathname pattern contains an unterminated '[' class".to_owned())?;
    if closing == content_start {
        return Err("pathname pattern contains an empty character class".to_owned());
    }

    let mut ranges = Vec::new();
    let mut index = content_start;
    while index < closing {
        if index + 2 < closing && active[index + 1] && units[index + 1] == ascii('-') {
            let start = units[index];
            let end = units[index + 2];
            if start > end {
                return Err("pathname pattern contains a descending character range".to_owned());
            }
            ranges.push((start, end));
            index += 3;
        } else {
            ranges.push((units[index], units[index]));
            index += 1;
        }
    }
    Ok((PatternToken::Class { negated, ranges }, closing + 1))
}

fn matches_component(pattern: &[PatternToken], candidate: &[NativeUnit]) -> bool {
    if candidate.first() == Some(&ascii('.'))
        && !matches!(pattern.first(), Some(PatternToken::Literal(unit)) if *unit == ascii('.'))
    {
        return false;
    }
    let mut states = vec![false; candidate.len() + 1];
    states[0] = true;
    for token in pattern {
        let mut next = vec![false; candidate.len() + 1];
        if matches!(token, PatternToken::AnySequence) {
            let mut reachable = false;
            for (index, state) in states.iter().copied().enumerate() {
                reachable |= state;
                next[index] = reachable;
            }
        } else {
            for index in 0..candidate.len() {
                if states[index] && token.matches(candidate[index]) {
                    next[index + 1] = true;
                }
            }
        }
        states = next;
    }
    states[candidate.len()]
}

#[cfg(unix)]
fn contains_pattern_introducer(value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt as _;

    value
        .as_bytes()
        .iter()
        .copied()
        .any(|unit| unit == ascii('*') || unit == ascii('?') || unit == ascii('['))
}

#[cfg(not(any(unix, windows)))]
fn contains_pattern_introducer(value: &OsStr) -> bool {
    value
        .to_string_lossy()
        .chars()
        .any(|unit| unit == '*' || unit == '?' || unit == '[')
}

#[cfg(unix)]
fn native_unit_len(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt as _;

    value.as_bytes().len()
}

#[cfg(windows)]
fn native_unit_len(value: &OsStr) -> usize {
    use std::os::windows::ffi::OsStrExt as _;

    value.encode_wide().count()
}

#[cfg(not(any(unix, windows)))]
fn native_unit_len(value: &OsStr) -> usize {
    value.to_string_lossy().chars().count()
}

#[cfg(unix)]
fn native_units(value: &OsStr) -> Vec<NativeUnit> {
    use std::os::unix::ffi::OsStrExt as _;

    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn native_units(value: &OsStr) -> Vec<NativeUnit> {
    use std::os::windows::ffi::OsStrExt as _;

    value.encode_wide().collect()
}

#[cfg(not(any(unix, windows)))]
fn native_units(value: &OsStr) -> Vec<NativeUnit> {
    value.to_string_lossy().chars().collect()
}

#[cfg(unix)]
const fn ascii(character: char) -> NativeUnit {
    character as u8
}

#[cfg(windows)]
const fn ascii(character: char) -> NativeUnit {
    character as u16
}

#[cfg(not(any(unix, windows)))]
const fn ascii(character: char) -> NativeUnit {
    character
}

#[cfg(windows)]
fn is_separator(unit: NativeUnit) -> bool {
    unit == ascii('/') || unit == ascii('\\')
}

#[cfg(not(windows))]
fn is_separator(unit: NativeUnit) -> bool {
    unit == ascii('/')
}

fn compare_native(left: &OsStr, right: &OsStr) -> Ordering {
    native_units(left).cmp(&native_units(right))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        MAX_PATHNAME_EXPANSION_ENTRIES, MAX_PATHNAME_EXPANSION_MATCHES, MAX_PATHNAME_PATTERN_UNITS,
        PathnameExpansionBudget, PathnameExpansionErrorKind, compile_component,
        compile_path_pattern, expand_pathnames, expand_pathnames_with_budget, matches_component,
        native_units,
    };
    use crate::expand::expand_word_with_substitutions;
    use crate::{ShellState, parse};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("ash-glob-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create glob test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn pathname_fields(source: &str, state: &ShellState) -> Vec<crate::expand::ExpandedWord> {
        let script = parse(source).expect("parse pathname fixture");
        expand_word_with_substitutions(&script.commands()[0].words()[1], state, std::iter::empty())
    }

    fn expanded(
        source: &str,
        state: &ShellState,
    ) -> Result<Vec<OsString>, super::PathnameExpansionError> {
        expand_pathnames(pathname_fields(source, state), state.cwd()).map(|words| {
            words
                .into_iter()
                .map(crate::expand::ExpandedWord::into_value)
                .collect()
        })
    }

    #[test]
    fn expands_sorted_matches_with_classes_and_hidden_file_rules() {
        let directory = TestDirectory::new();
        for path in ["beta.txt", "alpha.txt", "alpha.log", ".hidden.txt"] {
            fs::write(directory.path().join(path), b"").expect("glob fixture");
        }
        fs::create_dir_all(directory.path().join("one/two")).expect("nested glob fixture");
        fs::write(directory.path().join("one/top.txt"), b"").expect("one-level fixture");
        fs::write(directory.path().join("one/two/nested.txt"), b"").expect("nested fixture");
        let mut state = ShellState::new(directory.path());
        state
            .set_variable(
                "ABSOLUTE_PATTERN",
                directory.path().join("*.txt").into_os_string(),
            )
            .expect("absolute pattern variable");

        assert_eq!(
            expanded("echo *.txt", &state).expect("star expansion"),
            [OsString::from("alpha.txt"), OsString::from("beta.txt")]
        );
        assert_eq!(
            expanded("echo [a-b]????.txt", &state).expect("class expansion"),
            [OsString::from("alpha.txt")]
        );
        assert_eq!(
            expanded("echo [!b]*.txt", &state).expect("negated class expansion"),
            [OsString::from("alpha.txt")]
        );
        assert_eq!(
            expanded("echo .*.txt", &state).expect("explicit hidden expansion"),
            [OsString::from(".hidden.txt")]
        );
        assert_eq!(
            expanded("echo **/*.txt", &state).expect("double-star expansion"),
            [PathBuf::from("one/top.txt").into_os_string()]
        );
        assert_eq!(
            expanded("echo $ABSOLUTE_PATTERN", &state).expect("absolute expansion"),
            [
                directory.path().join("alpha.txt").into_os_string(),
                directory.path().join("beta.txt").into_os_string(),
            ]
        );
    }

    #[test]
    fn quotes_escapes_and_unquoted_parameters_control_pathname_operators() {
        let directory = TestDirectory::new();
        fs::write(directory.path().join("match.txt"), b"").expect("glob fixture");
        let mut state = ShellState::new(directory.path());
        state
            .set_variable("PATTERN", "*.txt")
            .expect("pattern variable");

        assert_eq!(
            expanded("echo $PATTERN", &state).expect("unquoted parameter expansion"),
            [OsString::from("match.txt")]
        );
        assert_eq!(
            expanded("echo \"$PATTERN\"", &state).expect("quoted parameter"),
            [OsString::from("*.txt")]
        );
        assert_eq!(
            expanded("echo \\*.txt", &state).expect("escaped star"),
            [OsString::from("*.txt")]
        );
    }

    #[test]
    fn rejects_no_match_and_malformed_or_descending_classes() {
        let directory = TestDirectory::new();
        let state = ShellState::new(directory.path());
        assert_eq!(
            expanded("echo *.missing", &state)
                .expect_err("no match")
                .kind(),
            PathnameExpansionErrorKind::NoMatches
        );
        assert_eq!(
            expanded("echo [abc", &state)
                .expect_err("unterminated class")
                .kind(),
            PathnameExpansionErrorKind::InvalidPattern
        );
        assert_eq!(
            expanded("echo []", &state).expect_err("empty class").kind(),
            PathnameExpansionErrorKind::InvalidPattern
        );
        assert_eq!(
            expanded("echo [z-a]", &state)
                .expect_err("descending class")
                .kind(),
            PathnameExpansionErrorKind::InvalidPattern
        );

        let mut state = state;
        state
            .set_variable(
                "LONG_PATTERN",
                format!("*{}", "a".repeat(MAX_PATHNAME_PATTERN_UNITS)),
            )
            .expect("long pattern variable");
        assert_eq!(
            expanded("echo $LONG_PATTERN", &state)
                .expect_err("pattern unit limit")
                .kind(),
            PathnameExpansionErrorKind::ResourceLimit
        );
    }

    #[test]
    fn shares_pattern_match_and_directory_entry_budgets_across_one_expansion_set() {
        let directory = TestDirectory::new();
        fs::write(directory.path().join("match"), b"").expect("glob fixture");
        let mut state = ShellState::new(directory.path());
        let script = parse("echo *").expect("parse budget fixture");
        let fields = || {
            expand_word_with_substitutions(
                &script.commands()[0].words()[1],
                &state,
                std::iter::empty(),
            )
        };

        let mut match_budget = PathnameExpansionBudget {
            pattern_units: 0,
            inspected_entries: 0,
            matches: MAX_PATHNAME_EXPANSION_MATCHES,
        };
        assert_eq!(
            expand_pathnames_with_budget(fields(), state.cwd(), &mut match_budget)
                .expect_err("shared match limit")
                .kind(),
            PathnameExpansionErrorKind::ResourceLimit
        );

        let mut entry_budget = PathnameExpansionBudget {
            pattern_units: 0,
            inspected_entries: MAX_PATHNAME_EXPANSION_ENTRIES,
            matches: 0,
        };
        assert_eq!(
            expand_pathnames_with_budget(fields(), state.cwd(), &mut entry_budget)
                .expect_err("shared entry limit")
                .kind(),
            PathnameExpansionErrorKind::ResourceLimit
        );

        let pattern = format!("*{}", "a".repeat(MAX_PATHNAME_PATTERN_UNITS / 2));
        state
            .set_variable("FIRST_PATTERN", &pattern)
            .expect("first shared pattern");
        state
            .set_variable("SECOND_PATTERN", pattern)
            .expect("second shared pattern");
        let first = pathname_fields("echo $FIRST_PATTERN", &state);
        let second = pathname_fields("echo $SECOND_PATTERN", &state);
        let mut pattern_budget = PathnameExpansionBudget::default();
        compile_path_pattern(&first[0], &mut pattern_budget).expect("first pattern within limit");
        assert_eq!(
            compile_path_pattern(&second[0], &mut pattern_budget)
                .expect_err("shared pattern unit limit")
                .kind(),
            PathnameExpansionErrorKind::ResourceLimit
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_prefix_mapping_keeps_verbatim_marker_literal_and_rejects_operators() {
        let mut state = ShellState::new(".");
        state
            .set_variable("PATTERN", r"\\?\C:\folder\file.txt")
            .expect("literal verbatim path");
        let fields = pathname_fields("echo $PATTERN", &state);
        let mut budget = PathnameExpansionBudget::default();
        let unchanged = expand_pathnames_with_budget(fields, state.cwd(), &mut budget)
            .expect("intrinsic verbatim marker is not a pattern");
        assert_eq!(budget.pattern_units, 0);
        assert_eq!(
            unchanged[0].value(),
            OsString::from(r"\\?\C:\folder\file.txt")
        );

        state
            .set_variable("PATTERN", r"\\?\C:\folder\*.txt")
            .expect("verbatim pattern");
        let fields = pathname_fields("echo $PATTERN", &state);
        let mut budget = PathnameExpansionBudget::default();
        let compiled = compile_path_pattern(&fields[0], &mut budget)
            .expect("verbatim prefix marker is platform syntax");
        assert!(compiled.has_pattern);

        state
            .set_variable("PATTERN", r"\\server*\share\folder\*.txt")
            .expect("UNC operator pattern");
        let fields = pathname_fields("echo $PATTERN", &state);
        assert_eq!(
            compile_path_pattern(&fields[0], &mut PathnameExpansionBudget::default())
                .expect_err("active UNC prefix operator")
                .kind(),
            PathnameExpansionErrorKind::InvalidPattern
        );
    }

    #[test]
    fn matcher_uses_native_units_and_fixed_case_sensitive_rules() {
        let pattern = native_units(OsString::from("a?*").as_os_str());
        let active = vec![true; pattern.len()];
        let tokens = compile_component(&pattern, &active)
            .expect("compile")
            .expect("operators");
        assert!(matches_component(
            &tokens,
            &native_units(OsString::from("ab").as_os_str())
        ));
        assert!(!matches_component(
            &tokens,
            &native_units(OsString::from("Ab").as_os_str())
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_matcher_accepts_non_utf8_native_units() {
        use std::os::unix::ffi::OsStringExt as _;

        let pattern = native_units(OsString::from("*").as_os_str());
        let tokens = compile_component(&pattern, &[true])
            .expect("compile")
            .expect("star operator");
        let candidate = native_units(OsString::from_vec(vec![0xff]).as_os_str());
        assert!(matches_component(&tokens, &candidate));
    }

    #[cfg(windows)]
    #[test]
    fn windows_matcher_accepts_unpaired_native_units() {
        use std::os::windows::ffi::OsStringExt as _;

        let pattern = native_units(OsString::from("?").as_os_str());
        let tokens = compile_component(&pattern, &[true])
            .expect("compile")
            .expect("question operator");
        let candidate = native_units(OsString::from_wide(&[0xd800]).as_os_str());
        assert!(matches_component(&tokens, &candidate));
    }
}
