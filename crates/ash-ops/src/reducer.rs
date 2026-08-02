use rayon::prelude::*;

const PARALLEL_LINE_THRESHOLD: usize = 2_048;
const PARTITION_LINES: usize = 1_024;
const BLOCK_CANDIDATE_BATCH_LINES: usize = 4_096;
const MAX_BLOCK_LINES: usize = 32;
const ERROR_CONTEXT_BEFORE: usize = 2;
const ERROR_CONTEXT_AFTER: usize = 6;
const ERROR_EDGE_LINES: usize = 2;
const REPEAT_SYMBOL: char = '×';
const OMISSION_SYMBOL: char = '⋯';
const ERROR_ANCHOR_TERMS: &[&[u8]] = &[
    b"error",
    b"fatal",
    b"panic",
    b"panicked",
    b"failed",
    b"failure",
    b"failures",
    b"exception",
    b"traceback",
    b"abort",
    b"aborted",
    b"assertion",
    b"segmentation fault",
    b"access violation",
    b"undefined reference",
    b"unhandled",
];

/// Deterministic projection retaining diagnostic windows from failed output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErrorFocusedReduction {
    text: String,
    diagnostic_lines: usize,
    omitted_spans: usize,
    omitted_lines: usize,
}

impl ErrorFocusedReduction {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn into_text(self) -> String {
        self.text
    }

    #[must_use]
    pub const fn reduced(&self) -> bool {
        self.omitted_spans != 0
    }

    #[must_use]
    pub const fn diagnostic_lines(&self) -> usize {
        self.diagnostic_lines
    }

    #[must_use]
    pub const fn omitted_spans(&self) -> usize {
        self.omitted_spans
    }

    #[must_use]
    pub const fn omitted_lines(&self) -> usize {
        self.omitted_lines
    }
}

/// Deterministic projection produced by consecutive-line reduction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepeatedLineReduction {
    text: String,
    collapsed_runs: usize,
    omitted_lines: usize,
}

/// Deterministic projection produced by consecutive-block reduction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepeatedBlockReduction {
    text: String,
    collapsed_blocks: usize,
    omitted_repetitions: usize,
    omitted_lines: usize,
}

impl RepeatedBlockReduction {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn into_text(self) -> String {
        self.text
    }

    #[must_use]
    pub const fn reduced(&self) -> bool {
        self.collapsed_blocks != 0
    }

    #[must_use]
    pub const fn collapsed_blocks(&self) -> usize {
        self.collapsed_blocks
    }

    #[must_use]
    pub const fn omitted_repetitions(&self) -> usize {
        self.omitted_repetitions
    }

    #[must_use]
    pub const fn omitted_lines(&self) -> usize {
        self.omitted_lines
    }
}

impl RepeatedLineReduction {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn into_text(self) -> String {
        self.text
    }

    #[must_use]
    pub const fn reduced(&self) -> bool {
        self.collapsed_runs != 0
    }

    #[must_use]
    pub const fn collapsed_runs(&self) -> usize {
        self.collapsed_runs
    }

    #[must_use]
    pub const fn omitted_lines(&self) -> usize {
        self.omitted_lines
    }
}

/// Collapses consecutive identical LF-terminated lines when `×N` is shorter.
///
/// `N` is the total run length, including the one line kept immediately before
/// the marker. Runs that would not reduce bytes are preserved verbatim. Rayon
/// partitions large inputs, then merges partition boundaries in source order.
#[must_use]
pub fn collapse_repeated_lines(text: &str) -> RepeatedLineReduction {
    let lines = text.split_inclusive('\n').collect::<Vec<_>>();
    if lines.len() < 2 {
        return unchanged(text);
    }

    let partitions = if lines.len() >= PARALLEL_LINE_THRESHOLD {
        lines
            .par_chunks(PARTITION_LINES)
            .enumerate()
            .map(|(partition, chunk)| local_runs(chunk, partition * PARTITION_LINES))
            .collect::<Vec<_>>()
    } else {
        vec![local_runs(&lines, 0)]
    };
    let runs = merge_runs(&lines, partitions);
    encode_runs(text, &lines, &runs)
}

/// Collapses consecutive repeated blocks as `B ×N#K` when that saves bytes.
///
/// `K` is the number of LF-terminated lines in the retained block and `N` is
/// the total repetition count. At each source position the maximum byte-saving
/// candidate wins; ties prefer smaller `K`, then larger `N`. Candidate search
/// uses ordered Rayon batches and exact bytes are verified before projection.
#[must_use]
pub fn collapse_repeated_blocks(text: &str) -> RepeatedBlockReduction {
    let layout = LineLayout::new(text);
    if layout.complete_lines < 4 {
        return unchanged_blocks(text);
    }
    let hashes = BlockHashes::new(&layout);
    let mut output = String::with_capacity(text.len());
    let mut collapsed_blocks = 0_usize;
    let mut omitted_repetitions = 0_usize;
    let mut omitted_lines = 0_usize;
    let mut index = 0_usize;

    while index < layout.total_lines() {
        if index >= layout.complete_lines {
            output.push_str(layout.line(index));
            index += 1;
            continue;
        }
        let batch_start = index;
        let batch_end = (batch_start + BLOCK_CANDIDATE_BATCH_LINES).min(layout.complete_lines);
        let candidates = if batch_end - batch_start >= PARALLEL_LINE_THRESHOLD {
            (batch_start..batch_end)
                .into_par_iter()
                .map(|start| best_hashed_candidate(&layout, &hashes, start))
                .collect::<Vec<_>>()
        } else {
            (batch_start..batch_end)
                .map(|start| best_hashed_candidate(&layout, &hashes, start))
                .collect::<Vec<_>>()
        };

        while index < batch_end {
            let hashed = candidates[index - batch_start];
            let candidate = verified_candidate(&layout, index, hashed);
            if candidate.present() {
                let block_lines = usize::from(candidate.block_lines);
                let repetitions = candidate.repetitions as usize;
                output.push_str(layout.span(index, block_lines));
                output.push_str(&block_marker(repetitions, block_lines));
                collapsed_blocks += 1;
                omitted_repetitions += repetitions - 1;
                omitted_lines += block_lines * (repetitions - 1);
                index += block_lines * repetitions;
                continue;
            }
            output.push_str(layout.line(index));
            index += 1;
        }
    }

    RepeatedBlockReduction {
        text: output,
        collapsed_blocks,
        omitted_repetitions,
        omitted_lines,
    }
}

/// Retains deterministic diagnostic windows and replaces byte-saving gaps.
///
/// The first and last two logical lines are always retained. Every recognized
/// diagnostic anchor retains two preceding and six following lines. A maximal
/// omitted gap of `N` lines becomes `⋯N` only when the marker is shorter than
/// the source gap. Large line classification enters Rayon, while selection and
/// encoding remain in source order and therefore worker-stable.
#[must_use]
pub fn focus_error_output(text: &str) -> ErrorFocusedReduction {
    let layout = LineLayout::new(text);
    let total_lines = layout.total_lines();
    if total_lines == 0 {
        return unchanged_error_focus(text, 0);
    }
    let anchors = if total_lines >= PARALLEL_LINE_THRESHOLD {
        (0..total_lines)
            .into_par_iter()
            .map(|line| is_diagnostic_anchor(layout.line(line)))
            .collect::<Vec<_>>()
    } else {
        (0..total_lines)
            .map(|line| is_diagnostic_anchor(layout.line(line)))
            .collect::<Vec<_>>()
    };
    let diagnostic_lines = anchors.iter().filter(|&&anchor| anchor).count();
    if diagnostic_lines == 0 {
        return unchanged_error_focus(text, 0);
    }

    let mut retained = vec![false; total_lines];
    let edge_lines = ERROR_EDGE_LINES.min(total_lines);
    retained[..edge_lines].fill(true);
    retained[total_lines - edge_lines..].fill(true);
    for (line, &anchor) in anchors.iter().enumerate() {
        if !anchor {
            continue;
        }
        let start = line.saturating_sub(ERROR_CONTEXT_BEFORE);
        let end = line
            .saturating_add(ERROR_CONTEXT_AFTER + 1)
            .min(total_lines);
        retained[start..end].fill(true);
    }

    let mut output = String::with_capacity(text.len());
    let mut omitted_spans = 0_usize;
    let mut omitted_lines = 0_usize;
    let mut line = 0_usize;
    while line < total_lines {
        if retained[line] {
            output.push_str(layout.line(line));
            line += 1;
            continue;
        }
        let start = line;
        while line < total_lines && !retained[line] {
            line += 1;
        }
        let lines = line - start;
        let source_bytes = layout.span_len(start, lines);
        if omission_marker_len(lines) < source_bytes {
            output.push_str(&omission_marker(lines));
            omitted_spans += 1;
            omitted_lines += lines;
        } else {
            output.push_str(layout.span(start, lines));
        }
    }

    ErrorFocusedReduction {
        text: output,
        diagnostic_lines,
        omitted_spans,
        omitted_lines,
    }
}

fn is_diagnostic_anchor(line: &str) -> bool {
    let bytes = line.as_bytes();
    ERROR_ANCHOR_TERMS
        .iter()
        .any(|term| contains_ascii_term(bytes, term))
        || bytes
            .split(|byte| !is_identifier_byte(*byte))
            .any(|word| word.ends_with(b"Error") || word.ends_with(b"Exception"))
}

fn contains_ascii_term(bytes: &[u8], term: &[u8]) -> bool {
    if term.is_empty() || term.len() > bytes.len() {
        return false;
    }
    bytes.windows(term.len()).enumerate().any(|(start, word)| {
        word.eq_ignore_ascii_case(term)
            && (start == 0 || !is_identifier_byte(bytes[start - 1]))
            && (start + term.len() == bytes.len() || !is_identifier_byte(bytes[start + term.len()]))
    })
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn omission_marker(lines: usize) -> String {
    format!("{OMISSION_SYMBOL}{lines}\n")
}

fn omission_marker_len(lines: usize) -> usize {
    OMISSION_SYMBOL.len_utf8() + decimal_digits(lines) + 1
}

struct LineLayout<'a> {
    text: &'a str,
    ends: Vec<usize>,
    complete_lines: usize,
}

impl<'a> LineLayout<'a> {
    fn new(text: &'a str) -> Self {
        let mut ends = text
            .match_indices('\n')
            .map(|(offset, _)| offset + 1)
            .collect::<Vec<_>>();
        let complete_lines = ends.len();
        if !text.is_empty() && ends.last().copied() != Some(text.len()) {
            ends.push(text.len());
        }
        Self {
            text,
            ends,
            complete_lines,
        }
    }

    fn total_lines(&self) -> usize {
        self.ends.len()
    }

    fn line_start(&self, line: usize) -> usize {
        if line == 0 { 0 } else { self.ends[line - 1] }
    }

    fn line(&self, line: usize) -> &'a str {
        self.span(line, 1)
    }

    fn span(&self, start: usize, lines: usize) -> &'a str {
        let byte_start = self.line_start(start);
        let byte_end = self.ends[start + lines - 1];
        &self.text[byte_start..byte_end]
    }

    fn span_len(&self, start: usize, lines: usize) -> usize {
        self.ends[start + lines - 1] - self.line_start(start)
    }
}

struct BlockHashes {
    first: Vec<u64>,
    second: Vec<u64>,
}

impl BlockHashes {
    fn new(layout: &LineLayout<'_>) -> Self {
        let mut first: Vec<u64> = Vec::with_capacity(layout.complete_lines + 1);
        let mut second: Vec<u64> = Vec::with_capacity(layout.complete_lines + 1);
        first.push(0);
        second.push(0);
        for line in 0..layout.complete_lines {
            let bytes = layout.line(line).as_bytes();
            let left = line_fingerprint(bytes, 0x9e37_79b1_85eb_ca87);
            let right = line_fingerprint(bytes, 0xc2b2_ae3d_27d4_eb4f);
            first.push(first.last().copied().unwrap_or_default().rotate_left(1) ^ left);
            second.push(second.last().copied().unwrap_or_default().rotate_left(23) ^ right);
        }
        Self { first, second }
    }

    fn ranges_equal(&self, left: usize, right: usize, lines: usize) -> bool {
        range_hash(&self.first, left, lines, 1) == range_hash(&self.first, right, lines, 1)
            && range_hash(&self.second, left, lines, 23)
                == range_hash(&self.second, right, lines, 23)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BlockCandidate {
    block_lines: u16,
    repetitions: u32,
    savings: usize,
}

impl BlockCandidate {
    const NONE: Self = Self {
        block_lines: 0,
        repetitions: 0,
        savings: 0,
    };

    const fn present(self) -> bool {
        self.block_lines != 0
    }
}

fn best_hashed_candidate(
    layout: &LineLayout<'_>,
    hashes: &BlockHashes,
    start: usize,
) -> BlockCandidate {
    let remaining = layout.complete_lines - start;
    let max_block_lines = MAX_BLOCK_LINES.min(remaining / 2);
    let mut best = BlockCandidate::NONE;
    for block_lines in 2..=max_block_lines {
        if !hashes.ranges_equal(start, start + block_lines, block_lines) {
            continue;
        }
        let lcp = hashed_longest_common_prefix(layout, hashes, start, block_lines);
        let repetitions = 1 + lcp / block_lines;
        if let Some(candidate) = block_candidate(layout, start, block_lines, repetitions) {
            best = preferred_candidate(best, candidate);
        }
    }
    best
}

fn hashed_longest_common_prefix(
    layout: &LineLayout<'_>,
    hashes: &BlockHashes,
    start: usize,
    block_lines: usize,
) -> usize {
    let mut low = block_lines;
    let mut high = layout.complete_lines - start - block_lines;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if hashes.ranges_equal(start, start + block_lines, middle) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

fn best_exact_candidate(layout: &LineLayout<'_>, start: usize) -> BlockCandidate {
    let remaining = layout.complete_lines - start;
    let max_block_lines = MAX_BLOCK_LINES.min(remaining / 2);
    let mut best = BlockCandidate::NONE;
    for block_lines in 2..=max_block_lines {
        let mut repetitions = 1_usize;
        while start + (repetitions + 1) * block_lines <= layout.complete_lines
            && layout.span(start, block_lines)
                == layout.span(start + repetitions * block_lines, block_lines)
        {
            repetitions += 1;
        }
        if let Some(candidate) = block_candidate(layout, start, block_lines, repetitions) {
            best = preferred_candidate(best, candidate);
        }
    }
    best
}

fn block_candidate(
    layout: &LineLayout<'_>,
    start: usize,
    block_lines: usize,
    repetitions: usize,
) -> Option<BlockCandidate> {
    if repetitions < 2 {
        return None;
    }
    let consumed_lines = block_lines.checked_mul(repetitions)?;
    if start.checked_add(consumed_lines)? > layout.complete_lines {
        return None;
    }
    let omitted_bytes = layout.span_len(start + block_lines, consumed_lines - block_lines);
    let marker_bytes = block_marker_len(repetitions, block_lines);
    if marker_bytes >= omitted_bytes {
        return None;
    }
    Some(BlockCandidate {
        block_lines: u16::try_from(block_lines).ok()?,
        repetitions: u32::try_from(repetitions).ok()?,
        savings: omitted_bytes - marker_bytes,
    })
}

fn preferred_candidate(current: BlockCandidate, candidate: BlockCandidate) -> BlockCandidate {
    if !current.present()
        || candidate.savings > current.savings
        || candidate.savings == current.savings
            && (candidate.block_lines < current.block_lines
                || candidate.block_lines == current.block_lines
                    && candidate.repetitions > current.repetitions)
    {
        candidate
    } else {
        current
    }
}

fn candidate_is_exact(layout: &LineLayout<'_>, start: usize, candidate: BlockCandidate) -> bool {
    let block_lines = usize::from(candidate.block_lines);
    let repetitions = candidate.repetitions as usize;
    let matches = |repetition| {
        layout.span(start, block_lines)
            == layout.span(start + repetition * block_lines, block_lines)
    };
    if block_lines * repetitions >= PARALLEL_LINE_THRESHOLD {
        (1..repetitions).into_par_iter().all(matches)
    } else {
        (1..repetitions).all(matches)
    }
}

fn verified_candidate(
    layout: &LineLayout<'_>,
    start: usize,
    candidate: BlockCandidate,
) -> BlockCandidate {
    if candidate.present() && candidate_is_exact(layout, start, candidate) {
        candidate
    } else if candidate.present() {
        best_exact_candidate(layout, start)
    } else {
        BlockCandidate::NONE
    }
}

fn range_hash(prefix: &[u64], start: usize, lines: usize, rotation: u32) -> u64 {
    let shift = ((lines & 63) as u32 * rotation) & 63;
    prefix[start + lines] ^ prefix[start].rotate_left(shift)
}

fn line_fingerprint(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = seed ^ (bytes.len() as u64).wrapping_mul(0x9e37_79b1_85eb_ca87);
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        hash ^= hash >> 29;
    }
    hash ^ hash.rotate_left(31)
}

fn block_marker(repetitions: usize, block_lines: usize) -> String {
    format!("{REPEAT_SYMBOL}{repetitions}#{block_lines}\n")
}

fn block_marker_len(repetitions: usize, block_lines: usize) -> usize {
    REPEAT_SYMBOL.len_utf8() + decimal_digits(repetitions) + decimal_digits(block_lines) + 2
}

fn decimal_digits(mut value: usize) -> usize {
    let mut digits = 1_usize;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

#[derive(Clone, Copy)]
struct Run {
    line: usize,
    count: usize,
}

fn local_runs(lines: &[&str], base: usize) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (offset, line) in lines.iter().enumerate() {
        match runs.last_mut() {
            Some(run) if lines[run.line - base] == *line => run.count += 1,
            _ => runs.push(Run {
                line: base + offset,
                count: 1,
            }),
        }
    }
    runs
}

fn merge_runs(lines: &[&str], partitions: Vec<Vec<Run>>) -> Vec<Run> {
    let mut merged: Vec<Run> = Vec::new();
    for partition in partitions {
        for run in partition {
            match merged.last_mut() {
                Some(previous) if lines[previous.line] == lines[run.line] => {
                    previous.count += run.count;
                }
                _ => merged.push(run),
            }
        }
    }
    merged
}

fn encode_runs(text: &str, lines: &[&str], runs: &[Run]) -> RepeatedLineReduction {
    let mut output = String::with_capacity(text.len());
    let mut collapsed_runs = 0_usize;
    let mut omitted_lines = 0_usize;
    for run in runs {
        let line = lines[run.line];
        if run.count == 1 {
            output.push_str(line);
            continue;
        }
        let marker = format!("{REPEAT_SYMBOL}{}\n", run.count);
        let omitted_bytes = line.len().saturating_mul(run.count - 1);
        if line.ends_with('\n') && marker.len() < omitted_bytes {
            output.push_str(line);
            output.push_str(&marker);
            collapsed_runs += 1;
            omitted_lines += run.count - 1;
        } else {
            for _ in 0..run.count {
                output.push_str(line);
            }
        }
    }
    RepeatedLineReduction {
        text: output,
        collapsed_runs,
        omitted_lines,
    }
}

fn unchanged(text: &str) -> RepeatedLineReduction {
    RepeatedLineReduction {
        text: text.to_owned(),
        collapsed_runs: 0,
        omitted_lines: 0,
    }
}

fn unchanged_blocks(text: &str) -> RepeatedBlockReduction {
    RepeatedBlockReduction {
        text: text.to_owned(),
        collapsed_blocks: 0,
        omitted_repetitions: 0,
        omitted_lines: 0,
    }
}

fn unchanged_error_focus(text: &str, diagnostic_lines: usize) -> ErrorFocusedReduction {
    ErrorFocusedReduction {
        text: text.to_owned(),
        diagnostic_lines,
        omitted_spans: 0,
        omitted_lines: 0,
    }
}

#[cfg(test)]
mod tests {
    use rayon::ThreadPoolBuilder;

    use super::{
        BlockHashes, LineLayout, best_hashed_candidate, collapse_repeated_blocks,
        collapse_repeated_lines, focus_error_output, verified_candidate,
    };

    fn block(prefix: &str, lines: usize) -> String {
        (0..lines)
            .map(|line| format!("{prefix}-frame-{line:02}\n"))
            .collect()
    }

    #[test]
    fn error_focus_keeps_edges_and_diagnostic_context_in_source_order() {
        let mut input = "setup\ncommand\n".to_owned();
        for line in 0..10 {
            input.push_str(&format!("before-{line:02}\n"));
        }
        input.push_str("error[E0425]: missing value\n");
        for line in 0..6 {
            input.push_str(&format!("detail-{line:02}\n"));
        }
        for line in 0..12 {
            input.push_str(&format!("after-{line:02}\n"));
        }
        input.push_str("summary\ndone\n");

        let reduction = focus_error_output(&input);

        assert_eq!(
            reduction.text(),
            concat!(
                "setup\n",
                "command\n",
                "⋯8\n",
                "before-08\n",
                "before-09\n",
                "error[E0425]: missing value\n",
                "detail-00\n",
                "detail-01\n",
                "detail-02\n",
                "detail-03\n",
                "detail-04\n",
                "detail-05\n",
                "⋯12\n",
                "summary\n",
                "done\n",
            )
        );
        assert_eq!(reduction.diagnostic_lines(), 1);
        assert_eq!(reduction.omitted_spans(), 2);
        assert_eq!(reduction.omitted_lines(), 20);
    }

    #[test]
    fn error_focus_recognizes_exception_suffixes_without_substring_false_positives() {
        let mut input = "start\ncommand\n".to_owned();
        for line in 0..12 {
            input.push_str(&format!("noise-{line:02}\n"));
        }
        input.push_str("TypeError: value is not callable\nstack-a\nstack-b\nend\ndone\n");
        let reduction = focus_error_output(&input);
        assert_eq!(reduction.diagnostic_lines(), 1);
        assert!(reduction.reduced());
        assert!(
            reduction
                .text()
                .contains("TypeError: value is not callable")
        );

        let ordinary = "terror value\nfailover ready\nexceptional case\n";
        let unchanged = focus_error_output(ordinary);
        assert_eq!(unchanged.text(), ordinary);
        assert_eq!(unchanged.diagnostic_lines(), 0);
        assert!(!unchanged.reduced());
    }

    #[test]
    fn error_focus_is_worker_stable_and_preserves_an_unterminated_utf8_tail() {
        let mut input = String::new();
        for line in 0..8_192 {
            if line % 1_024 == 511 {
                input.push_str(&format!("fatal: shard {line:04} failed\n"));
            } else {
                input.push_str(&format!("构建记录-{line:04}\n"));
            }
        }
        input.push_str("最终摘要");
        let one = ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("one-worker pool")
            .install(|| focus_error_output(&input));
        let four = ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("four-worker pool")
            .install(|| focus_error_output(&input));

        assert_eq!(one, four);
        assert_eq!(one.diagnostic_lines(), 8);
        assert!(one.omitted_spans() > 8);
        assert!(one.text().ends_with("最终摘要"));
    }

    #[test]
    fn error_focus_never_expands_a_short_diagnostic_capture() {
        let input = "start\nerror: boom\nend\n";
        let reduction = focus_error_output(input);

        assert_eq!(reduction.text(), input);
        assert_eq!(reduction.diagnostic_lines(), 1);
        assert_eq!(reduction.omitted_spans(), 0);
        assert!(!reduction.reduced());
    }

    #[test]
    fn block_projection_prefers_the_smallest_maximum_saving_period() {
        let input = "alpha\nbeta\n".repeat(8);
        let reduction = collapse_repeated_blocks(&input);

        assert_eq!(reduction.text(), "alpha\nbeta\n×8#2\n");
        assert_eq!(reduction.collapsed_blocks(), 1);
        assert_eq!(reduction.omitted_repetitions(), 7);
        assert_eq!(reduction.omitted_lines(), 14);
    }

    #[test]
    fn block_projection_preserves_utf8_tail_and_non_saving_runs() {
        let utf8 = format!("{}尾部", "诊断\n位置\n".repeat(3));
        let reduced = collapse_repeated_blocks(&utf8);
        assert_eq!(reduced.text(), "诊断\n位置\n×3#2\n尾部");
        assert_eq!(reduced.collapsed_blocks(), 1);

        let short = "a\nb\na\nb\n";
        let unchanged = collapse_repeated_blocks(short);
        assert_eq!(unchanged.text(), short);
        assert!(!unchanged.reduced());
    }

    #[test]
    fn block_projection_is_worker_stable_across_candidate_batches() {
        let first = block("first", 7);
        let second = block("second", 5);
        let input = format!("{}{}", first.repeat(700), second.repeat(401));
        let expected = format!("{first}×700#7\n{second}×401#5\n");
        let one = ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("one-worker pool")
            .install(|| collapse_repeated_blocks(&input));
        let four = ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("four-worker pool")
            .install(|| collapse_repeated_blocks(&input));

        assert_eq!(one, four);
        assert_eq!(one.text(), expected);
        assert_eq!(one.collapsed_blocks(), 2);
        assert_eq!(one.omitted_repetitions(), 1_099);
        assert_eq!(one.omitted_lines(), 6_893);
    }

    #[test]
    fn fingerprint_collisions_cannot_authorize_block_omission() {
        let input = (0..128)
            .map(|line| format!("unique-frame-{line:03}\n"))
            .collect::<String>();
        let layout = LineLayout::new(&input);
        let colliding = BlockHashes {
            first: vec![0; layout.complete_lines + 1],
            second: vec![0; layout.complete_lines + 1],
        };
        let proposed = best_hashed_candidate(&layout, &colliding, 0);

        assert!(proposed.present());
        assert!(!verified_candidate(&layout, 0, proposed).present());
        assert_eq!(collapse_repeated_blocks(&input).text(), input);
    }

    #[test]
    fn partition_boundaries_merge_without_changing_order() {
        let mut input = String::new();
        for index in 0..4_096 {
            let group = index / 1_500;
            input.push_str(&format!("diagnostic-{group:02}\n"));
        }
        let one = ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("one-worker pool")
            .install(|| collapse_repeated_lines(&input));
        let four = ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("four-worker pool")
            .install(|| collapse_repeated_lines(&input));

        assert_eq!(one, four);
        assert_eq!(one.collapsed_runs(), 3);
        assert_eq!(one.omitted_lines(), 4_093);
        assert_eq!(
            one.text(),
            "diagnostic-00\n×1500\ndiagnostic-01\n×1500\ndiagnostic-02\n×1096\n"
        );
    }

    #[test]
    fn utf8_runs_collapse_without_touching_an_unterminated_tail() {
        let input = format!("{}尾部", "诊断\n".repeat(3));
        let reduction = collapse_repeated_lines(&input);

        assert_eq!(reduction.text(), "诊断\n×3\n尾部");
        assert_eq!(reduction.collapsed_runs(), 1);
        assert_eq!(reduction.omitted_lines(), 2);
    }
}
