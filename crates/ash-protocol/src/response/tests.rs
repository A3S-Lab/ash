use super::{
    CancelResult, CancellationState, ErrorCode, ErrorRecord, ErrorStage, FileKind, FinalResponse,
    ListEntry, PathMapping, ProcessResult, RESULT_RETAINED, RESULT_TRUNCATED, ReadResult,
    ResponseError, ResultData, RetryClass, SearchMatch, Status, StreamResult, TerminationKind,
};
use crate::ason::decode;

const SEARCH_RESULT: &str = include_str!("../../../../spec/fixtures/ason/search-result.ason");

#[test]
fn search_result_matches_the_canonical_specification_fixture() {
    let response = FinalResponse::success(
        17,
        vec![PathMapping {
            id: 1,
            value: "src/lib.rs".to_owned(),
        }],
        ResultData::Search(vec![
            SearchMatch {
                path: 1,
                line: 42,
                column: 7,
                text: "TODO item".to_owned(),
            },
            SearchMatch {
                path: 1,
                line: 87,
                column: 3,
                text: "FIXME item".to_owned(),
            },
        ]),
        0,
        None,
    )
    .expect("response");
    let encoded = response.encode().expect("encode").encode();
    assert_eq!(encoded, SEARCH_RESULT);
    assert_eq!(decode(&encoded).expect("ASON").encode(), encoded);
}

#[test]
fn every_m1_result_shape_encodes_as_canonical_ason() {
    let results = [
        ResultData::Exec(ProcessResult {
            termination: TerminationKind::Exited,
            code: Some(0),
            elapsed_millis: 9,
            stdout: StreamResult {
                projection: Some("ok\n".to_owned()),
                reference: None,
            },
            stderr: StreamResult::default(),
        }),
        ResultData::Read(vec![ReadResult {
            path: 1,
            offset: 1,
            length: 2,
            digest: "a".repeat(64),
            text: Some("a\nb\n".to_owned()),
            reference: None,
        }]),
        ResultData::List(vec![ListEntry {
            path: 1,
            kind: FileKind::File,
            size: 4,
            modified_millis: None,
        }]),
        ResultData::Search(vec![]),
        ResultData::Cancel(CancelResult {
            target_id: 7,
            state: CancellationState::Signaled,
        }),
    ];
    for (index, data) in results.into_iter().enumerate() {
        let response =
            FinalResponse::success(index as u64 + 1, vec![], data, 0, None).expect("response");
        let encoded = response.encode().expect("encode").encode();
        assert_eq!(
            decode(&encoded).expect("canonical result").encode(),
            encoded
        );
    }
}

#[test]
fn errors_are_structural_and_may_carry_partial_typed_data() {
    let response = FinalResponse::failure(
        21,
        Status::Failed,
        ErrorRecord {
            code: ErrorCode::ProcessFailed,
            retry: RetryClass::Never,
            stage: ErrorStage::Execute,
            evidence: Some(8),
            argument: None,
        },
        vec![],
        Some(ResultData::Exec(ProcessResult {
            termination: TerminationKind::Exited,
            code: Some(1),
            elapsed_millis: 12,
            stdout: StreamResult::default(),
            stderr: StreamResult {
                projection: Some("failed".to_owned()),
                reference: Some(8),
            },
        })),
        RESULT_TRUNCATED | RESULT_RETAINED,
        None,
    )
    .expect("response");
    let encoded = response.encode().expect("encode").encode();
    assert!(encoded.contains("e{c,q,p,x,a}:\n401,0,4,@8,~\n"));
    assert_eq!(decode(&encoded).expect("ASON").encode(), encoded);
}

#[test]
fn truncation_without_any_reference_is_rejected() {
    let response = FinalResponse::success(
        1,
        vec![],
        ResultData::Search(vec![]),
        RESULT_TRUNCATED,
        None,
    );
    assert_eq!(response, Err(ResponseError::MissingRetainedReference));
}
