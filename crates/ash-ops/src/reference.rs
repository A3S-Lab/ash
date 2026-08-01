use ash_engine::{PermitKind, Program};
use ash_protocol::request::{REF_CASE_INSENSITIVE, REF_REGEX, RefArgs, RefMode, Request};
use ash_protocol::response::{
    FinalResponse, RESULT_NORMALIZED_TEXT, RESULT_REDUCED, RESULT_RETAINED, RESULT_TRUNCATED,
    ReferenceMatch, ReferenceResult, ReferenceSlice, ReleasedReference, ResultData,
};
use regex::{Regex, RegexBuilder};

use crate::OperationError;
use crate::projection::{charge, largest_prefix, presentation_limit};

const MAX_REFERENCE_MATCHES: usize = 1_000_000;
const MAX_PROJECTED_LINE_BYTES: usize = 4 * 1024;
const MAX_MATCH_PROJECTION_BYTES: usize = 64 * 1024 * 1024;

pub async fn execute(
    request: &Request,
    arguments: &RefArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    check_cancelled(program)?;
    if arguments.mode() == RefMode::Release {
        return release(request, arguments, program);
    }

    let bytes = program.store().get(arguments.reference())?;
    let _lease = std::sync::Arc::clone(&bytes);
    let _compute = program.acquire(PermitKind::Compute).await?;
    let cancellation = program.cancellation().clone();
    let offset = arguments.offset();
    let length = arguments.length();
    let result = match arguments.mode() {
        RefMode::Bytes => program
            .compute_pool()
            .run(move || select_bytes(&bytes, offset, length))
            .await?
            .map(RawResult::Slice)?,
        RefMode::Lines => program
            .compute_pool()
            .run(move || select_lines(&bytes, offset, length, &cancellation))
            .await?
            .map(RawResult::Slice)?,
        RefMode::Search => {
            let query = arguments
                .query()
                .ok_or(OperationError::WorkLimit)?
                .to_owned();
            let flags = arguments.flags();
            program
                .compute_pool()
                .run(move || search_bytes(&bytes, offset, length, &query, flags, &cancellation))
                .await?
                .map(RawResult::Search)?
        }
        RefMode::Release => return Err(OperationError::WorkLimit),
    };
    check_cancelled(program)?;
    match result {
        RawResult::Slice(slice) => slice_response(request, arguments.reference(), slice, program),
        RawResult::Search(matches) => {
            search_response(request, arguments.reference(), &matches, program)
        }
    }
}

enum RawResult {
    Slice(RawSlice),
    Search(Vec<RawMatch>),
}

struct RawSlice {
    offset: u64,
    length: u64,
    bytes: Vec<u8>,
    digest: String,
}

fn select_bytes(bytes: &[u8], offset: u64, length: u64) -> Result<RawSlice, OperationError> {
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let requested = usize::try_from(length).unwrap_or(usize::MAX);
    let end = start.saturating_add(requested).min(bytes.len());
    raw_slice(start as u64, &bytes[start..end])
}

fn select_lines(
    bytes: &[u8],
    offset: u64,
    length: u64,
    cancellation: &ash_engine::CancellationToken,
) -> Result<RawSlice, OperationError> {
    if bytes.is_empty() {
        return raw_slice(offset, &[]);
    }
    let target = offset.saturating_sub(1);
    let mut cursor = 0_usize;
    let mut start = None;
    let mut selected = 0_u64;
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if index % 4096 == 0 && cancellation.is_cancelled() {
            return Err(OperationError::Cancelled);
        }
        let line_index = index as u64;
        if line_index == target {
            start = Some(cursor);
        }
        if line_index >= target && selected < length {
            selected += 1;
        }
        cursor += line.len();
        if selected == length {
            break;
        }
    }
    let Some(start) = start else {
        return raw_slice(offset, &[]);
    };
    raw_slice(offset, &bytes[start..cursor])
}

fn raw_slice(offset: u64, bytes: &[u8]) -> Result<RawSlice, OperationError> {
    Ok(RawSlice {
        offset,
        length: bytes.len() as u64,
        digest: blake3::hash(bytes).to_hex().to_string(),
        bytes: bytes.to_vec(),
    })
}

fn slice_response(
    request: &Request,
    reference: u64,
    slice: RawSlice,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    let limit = presentation_limit(program);
    let text = std::str::from_utf8(&slice.bytes).is_ok();
    let projection_ceiling = if text { limit } else { limit / 2 }.min(slice.bytes.len());
    let mut low = 0_usize;
    let mut high = projection_ceiling;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let projected = projection_boundary(&slice.bytes, middle, text);
        let response = make_slice_response(request.id(), reference, &slice, projected, text)?;
        if response.encode()?.encode().len() <= limit {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let projected = projection_boundary(&slice.bytes, low, text);
    let response = make_slice_response(request.id(), reference, &slice, projected, text)?;
    if response.encode()?.encode().len() > limit {
        return Err(OperationError::OutputBudget);
    }
    charge(program, &response, 1)?;
    Ok(response)
}

fn projection_boundary(bytes: &[u8], mut length: usize, text: bool) -> usize {
    length = length.min(bytes.len());
    if text {
        while length > 0 && std::str::from_utf8(&bytes[..length]).is_err() {
            length -= 1;
        }
    }
    length
}

fn make_slice_response(
    request_id: u64,
    reference: u64,
    slice: &RawSlice,
    projected: usize,
    text: bool,
) -> Result<FinalResponse, OperationError> {
    let prefix = &slice.bytes[..projected];
    let (text, hex) = if text {
        let projection = std::str::from_utf8(prefix)
            .map_err(|_| OperationError::WrongFileType)?
            .to_owned();
        (Some(projection), None)
    } else {
        (None, Some(hex(prefix)))
    };
    let truncated = projected < slice.bytes.len();
    let flags = RESULT_RETAINED
        | if truncated {
            RESULT_TRUNCATED | RESULT_REDUCED
        } else {
            0
        };
    Ok(FinalResponse::success(
        request_id,
        vec![],
        ResultData::Reference(ReferenceResult::Slice(ReferenceSlice {
            offset: slice.offset,
            length: slice.length,
            projected_bytes: projected as u64,
            digest: slice.digest.clone(),
            text,
            hex,
        })),
        flags,
        Some(reference),
    )?)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[derive(Clone)]
enum Matcher {
    Literal(String),
    Regex(Regex),
}

impl Matcher {
    fn new(query: &str, flags: u32) -> Result<Self, OperationError> {
        let regex = flags & REF_REGEX != 0;
        let insensitive = flags & REF_CASE_INSENSITIVE != 0;
        if regex || insensitive {
            let pattern = if regex {
                query.to_owned()
            } else {
                regex::escape(query)
            };
            Ok(Self::Regex(
                RegexBuilder::new(&pattern)
                    .case_insensitive(insensitive)
                    .size_limit(16 * 1024 * 1024)
                    .build()?,
            ))
        } else {
            Ok(Self::Literal(query.to_owned()))
        }
    }

    fn visit_matches<F>(&self, line: &str, mut visit: F) -> Result<(), OperationError>
    where
        F: FnMut(usize, usize) -> Result<(), OperationError>,
    {
        match self {
            Self::Literal(query) => {
                for (start, value) in line.match_indices(query) {
                    visit(start, start + value.len())?;
                }
            }
            Self::Regex(regex) => {
                for matched in regex.find_iter(line) {
                    visit(matched.start(), matched.end())?;
                }
            }
        }
        Ok(())
    }
}

struct RawMatch {
    offset: u64,
    line: u64,
    column: u64,
    text: String,
    normalized: bool,
    reduced: bool,
}

fn search_bytes(
    bytes: &[u8],
    offset: u64,
    length: u64,
    query: &str,
    flags: u32,
    cancellation: &ash_engine::CancellationToken,
) -> Result<Vec<RawMatch>, OperationError> {
    let text = std::str::from_utf8(bytes).map_err(|_| OperationError::WrongFileType)?;
    let matcher = Matcher::new(query, flags)?;
    let requested_start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let requested_end = requested_start
        .saturating_add(usize::try_from(length).unwrap_or(usize::MAX))
        .min(bytes.len());
    let range_start = ceil_char_boundary(text, requested_start);
    let range_end = floor_char_boundary(text, requested_end);
    let mut matches = Vec::new();
    let mut projected_bytes = 0_usize;
    let mut line_start = 0_usize;
    for (line_index, raw_line) in text.split_inclusive('\n').enumerate() {
        if line_index % 4096 == 0 && cancellation.is_cancelled() {
            return Err(OperationError::Cancelled);
        }
        if line_start >= range_end {
            break;
        }
        let without_lf = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line = without_lf.strip_suffix('\r').unwrap_or(without_lf);
        let normalized = line.len() != without_lf.len();
        matcher.visit_matches(line, |start, end| {
            let absolute_start = line_start.saturating_add(start);
            let absolute_end = line_start.saturating_add(end);
            if absolute_start >= range_start
                && absolute_start < range_end
                && absolute_end <= range_end
            {
                let (projection, reduced) = project_line(line, start, end);
                projected_bytes = projected_bytes
                    .checked_add(projection.len())
                    .ok_or(OperationError::WorkLimit)?;
                if projected_bytes > MAX_MATCH_PROJECTION_BYTES {
                    return Err(OperationError::WorkLimit);
                }
                matches.push(RawMatch {
                    offset: absolute_start as u64,
                    line: line_index as u64 + 1,
                    column: start as u64 + 1,
                    text: projection,
                    normalized,
                    reduced,
                });
                if matches.len() > MAX_REFERENCE_MATCHES {
                    return Err(OperationError::WorkLimit);
                }
            }
            Ok(())
        })?;
        line_start = line_start.saturating_add(raw_line.len());
    }
    Ok(matches)
}

fn project_line(line: &str, match_start: usize, match_end: usize) -> (String, bool) {
    if line.len() <= MAX_PROJECTED_LINE_BYTES {
        return (line.to_owned(), false);
    }
    let target = MAX_PROJECTED_LINE_BYTES.saturating_sub(6);
    let before = target / 2;
    let after = target.saturating_sub(before);
    let start = floor_char_boundary(line, match_start.saturating_sub(before));
    let mut end = ceil_char_boundary(line, match_end.saturating_add(after).min(line.len()));
    if end.saturating_sub(start) > target {
        end = floor_char_boundary(line, start.saturating_add(target));
    }
    let leading = start != 0;
    let trailing = end != line.len();
    let mut projection = String::with_capacity(end.saturating_sub(start).saturating_add(6));
    if leading {
        projection.push_str("...");
    }
    projection.push_str(&line[start..end]);
    if trailing {
        projection.push_str("...");
    }
    (projection, true)
}

fn search_response(
    request: &Request,
    reference: u64,
    matches: &[RawMatch],
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    let limit = presentation_limit(program);
    let record_limit = program.budget().remaining().records as usize;
    let prefix = largest_prefix(matches.len(), record_limit, limit, |length, truncated| {
        make_search_response(request.id(), reference, &matches[..length], truncated)
    })?;
    let response = make_search_response(
        request.id(),
        reference,
        &matches[..prefix],
        prefix < matches.len(),
    )?;
    charge(program, &response, prefix)?;
    Ok(response)
}

fn make_search_response(
    request_id: u64,
    reference: u64,
    matches: &[RawMatch],
    truncated: bool,
) -> Result<FinalResponse, OperationError> {
    let normalized = matches.iter().any(|entry| entry.normalized);
    let reduced = truncated || matches.iter().any(|entry| entry.reduced);
    let flags = RESULT_RETAINED
        | if normalized {
            RESULT_NORMALIZED_TEXT
        } else {
            0
        }
        | if reduced { RESULT_REDUCED } else { 0 }
        | if truncated { RESULT_TRUNCATED } else { 0 };
    Ok(FinalResponse::success(
        request_id,
        vec![],
        ResultData::Reference(ReferenceResult::Search(
            matches
                .iter()
                .map(|entry| ReferenceMatch {
                    offset: entry.offset,
                    line: entry.line,
                    column: entry.column,
                    text: entry.text.clone(),
                })
                .collect(),
        )),
        flags,
        Some(reference),
    )?)
}

fn release(
    request: &Request,
    arguments: &RefArgs,
    program: &Program,
) -> Result<FinalResponse, OperationError> {
    let response = FinalResponse::success(
        request.id(),
        vec![],
        ResultData::Reference(ReferenceResult::Released(ReleasedReference {
            reference: arguments.reference(),
        })),
        0,
        None,
    )?;
    if response.encode()?.encode().len() > presentation_limit(program) {
        return Err(OperationError::OutputBudget);
    }
    charge(program, &response, 1)?;
    program.store().release(arguments.reference())?;
    Ok(response)
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn check_cancelled(program: &Program) -> Result<(), OperationError> {
    if program.cancellation().is_cancelled() {
        Err(OperationError::Cancelled)
    } else {
        program.budget().check_deadline()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{hex, project_line};

    #[test]
    fn binary_projection_is_lowercase_hex_and_long_lines_keep_the_match() {
        assert_eq!(hex(&[0, 15, 16, 255]), "000f10ff");
        let line = format!("{}needle{}", "a".repeat(5_000), "b".repeat(5_000));
        let (projection, reduced) = project_line(&line, 5_000, 5_006);
        assert!(reduced);
        assert!(projection.contains("needle"));
        assert!(projection.len() <= 4_102);
    }
}
