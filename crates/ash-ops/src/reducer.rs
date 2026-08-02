use rayon::prelude::*;

const PARALLEL_LINE_THRESHOLD: usize = 2_048;
const PARTITION_LINES: usize = 1_024;
const REPEAT_SYMBOL: char = '×';

/// Deterministic projection produced by consecutive-line reduction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepeatedLineReduction {
    text: String,
    collapsed_runs: usize,
    omitted_lines: usize,
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

#[cfg(test)]
mod tests {
    use rayon::ThreadPoolBuilder;

    use super::collapse_repeated_lines;

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
