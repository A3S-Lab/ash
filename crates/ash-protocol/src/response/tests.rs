use super::{
    BatchNodeResult, BatchNodeState, CancelResult, CancellationState, ErrorCode, ErrorRecord,
    ErrorStage, FileKind, FinalResponse, FsResult, FsState, ListEntry, MaterializedReference,
    PatchResult, PatchState, PathMapping, ProcessResult, RESULT_REDUCED, RESULT_RETAINED,
    RESULT_TRUNCATED, ReadResult, ReferenceMatch, ReferenceResult, ReferenceSlice,
    ReleasedReference, ResponseError, ResultData, RetryClass, SearchMatch, SnapshotChange,
    SnapshotResult, Status, StreamResult, TerminationKind,
};
use crate::Operation;
use crate::ason::{Atom, Cell, Key, Table, decode};
use crate::request::FsActionKind;

const SEARCH_RESULT: &str = include_str!("../../../../spec/fixtures/ason/search-result.ason");
const BATCH_RESULT: &str = include_str!("../../../../spec/fixtures/ason/batch-result.ason");
const FS_RESULT: &str = include_str!("../../../../spec/fixtures/ason/fs-result.ason");
const REF_PROJECT_RESULT: &str =
    include_str!("../../../../spec/fixtures/ason/ref-project-result.ason");
const REF_MATERIALIZE_RESULT: &str =
    include_str!("../../../../spec/fixtures/ason/ref-materialize-result.ason");

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
fn every_core_result_shape_encodes_as_canonical_ason() {
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
        ResultData::Patch(vec![PatchResult {
            path: 1,
            state: PatchState::Committed,
            digest: Some("c".repeat(64)),
        }]),
        ResultData::Snapshot(vec![SnapshotResult {
            path: 1,
            change: SnapshotChange::Present,
            kind: FileKind::File,
            size: 4,
            digest: Some("e".repeat(64)),
        }]),
        ResultData::Reference(ReferenceResult::Slice(ReferenceSlice {
            offset: 4,
            length: 2,
            projected_bytes: 2,
            digest: "b".repeat(64),
            text: Some("ok".to_owned()),
            hex: None,
        })),
        ResultData::Cancel(CancelResult {
            target_id: 7,
            state: CancellationState::Signaled,
        }),
    ];
    for (index, data) in results.into_iter().enumerate() {
        let retained = matches!(&data, ResultData::Reference(_) | ResultData::Snapshot(_));
        let response = FinalResponse::success(
            index as u64 + 1,
            vec![],
            data,
            if retained { RESULT_RETAINED } else { 0 },
            retained.then_some(7),
        )
        .expect("response");
        let encoded = response.encode().expect("encode").encode();
        assert_eq!(
            decode(&encoded).expect("canonical result").encode(),
            encoded
        );
    }
}

#[test]
fn every_reference_result_shape_is_typed_and_source_bound() {
    let search = FinalResponse::success(
        31,
        vec![],
        ResultData::Reference(ReferenceResult::Search(vec![ReferenceMatch {
            offset: 8,
            line: 2,
            column: 3,
            text: "needle".to_owned(),
        }])),
        RESULT_RETAINED,
        Some(4),
    )
    .expect("search");
    let released = FinalResponse::success(
        32,
        vec![],
        ResultData::Reference(ReferenceResult::Released(ReleasedReference {
            reference: 4,
        })),
        0,
        None,
    )
    .expect("release");
    for response in [search, released] {
        let encoded = response.encode().expect("encode").encode();
        assert_eq!(decode(&encoded).expect("canonical").encode(), encoded);
    }

    assert_eq!(
        FinalResponse::success(
            33,
            vec![],
            ResultData::Reference(ReferenceResult::Search(vec![])),
            0,
            None,
        ),
        Err(ResponseError::InvalidData)
    );
}

#[test]
fn reference_formula_results_match_canonical_fixtures() {
    let projection = FinalResponse::success(
        44,
        vec![],
        ResultData::Reference(ReferenceResult::Projection(
            Table::new(
                ["p", "t"]
                    .into_iter()
                    .map(Key::new)
                    .collect::<Result<_, _>>()
                    .expect("columns"),
                vec![
                    vec![
                        Cell::Atom(Atom::text("src/a.rs")),
                        Cell::Atom(Atom::text("TODO")),
                    ],
                    vec![
                        Cell::Atom(Atom::text("src/b.rs")),
                        Cell::Atom(Atom::text("FIXME")),
                    ],
                ],
            )
            .expect("table"),
        )),
        RESULT_REDUCED | RESULT_RETAINED,
        Some(7),
    )
    .expect("projection response");
    assert_eq!(
        projection.encode().expect("encode").encode(),
        REF_PROJECT_RESULT
    );

    let materialized = FinalResponse::success(
        45,
        vec![PathMapping {
            id: 1,
            value: "artifacts/out.bin".to_owned(),
        }],
        ResultData::Reference(ReferenceResult::Materialized(MaterializedReference {
            path: 1,
            state: FsState::Committed,
            size: 3,
            digest: Some("a".repeat(64)),
        })),
        RESULT_RETAINED,
        Some(8),
    )
    .expect("materialized response");
    assert_eq!(
        materialized.encode().expect("encode").encode(),
        REF_MATERIALIZE_RESULT
    );

    for fixture in [REF_PROJECT_RESULT, REF_MATERIALIZE_RESULT] {
        assert_eq!(decode(fixture).expect("canonical").encode(), fixture);
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

#[test]
fn patch_status_and_per_file_states_must_agree() {
    let conflict = PatchResult {
        path: 1,
        state: PatchState::Conflict,
        digest: Some("d".repeat(64)),
    };
    assert_eq!(
        FinalResponse::success(
            40,
            vec![],
            ResultData::Patch(vec![conflict.clone()]),
            0,
            None,
        ),
        Err(ResponseError::InvalidData)
    );
    assert!(
        FinalResponse::failure(
            41,
            Status::Conflict,
            ErrorRecord {
                code: ErrorCode::ContentConflict,
                retry: RetryClass::CorrectRequest,
                stage: ErrorStage::Execute,
                evidence: None,
                argument: None,
            },
            vec![],
            Some(ResultData::Patch(vec![conflict])),
            0,
            None,
        )
        .is_ok()
    );
}

#[test]
fn snapshot_results_are_bound_to_a_retained_manifest() {
    assert_eq!(
        FinalResponse::success(50, vec![], ResultData::Snapshot(vec![]), 0, None,),
        Err(ResponseError::InvalidData)
    );
    assert!(
        FinalResponse::success(
            51,
            vec![],
            ResultData::Snapshot(vec![]),
            RESULT_RETAINED,
            Some(3),
        )
        .is_ok()
    );
}

#[test]
fn batch_results_are_compact_references_in_stable_node_order() {
    let response = FinalResponse::success(
        70,
        vec![],
        ResultData::Batch(vec![
            BatchNodeResult {
                id: 1,
                operation: Operation::Search,
                state: BatchNodeState::Succeeded,
                status: Some(Status::Success),
                reference: Some(4),
            },
            BatchNodeResult {
                id: 2,
                operation: Operation::Read,
                state: BatchNodeState::Succeeded,
                status: Some(Status::Success),
                reference: Some(5),
            },
        ]),
        RESULT_RETAINED,
        None,
    )
    .expect("batch response");
    assert_eq!(response.encode().expect("encode").encode(), BATCH_RESULT);
    assert_eq!(decode(BATCH_RESULT).expect("ASON").encode(), BATCH_RESULT);

    assert!(
        FinalResponse::failure(
            71,
            Status::Failed,
            ErrorRecord {
                code: ErrorCode::BatchFailed,
                retry: RetryClass::Never,
                stage: ErrorStage::Execute,
                evidence: None,
                argument: None,
            },
            vec![],
            Some(ResultData::Batch(vec![
                BatchNodeResult {
                    id: 1,
                    operation: Operation::Exec,
                    state: BatchNodeState::Failed,
                    status: Some(Status::Failed),
                    reference: Some(4),
                },
                BatchNodeResult {
                    id: 2,
                    operation: Operation::Read,
                    state: BatchNodeState::Skipped,
                    status: None,
                    reference: None,
                },
            ])),
            RESULT_RETAINED | super::RESULT_PARTIAL,
            None,
        )
        .is_ok()
    );
}

#[test]
fn filesystem_result_matches_the_canonical_transaction_fixture() {
    let response = FinalResponse::success(
        84,
        vec![
            PathMapping {
                id: 1,
                value: "new.txt".to_owned(),
            },
            PathMapping {
                id: 2,
                value: "Cargo.toml".to_owned(),
            },
            PathMapping {
                id: 3,
                value: "Cargo.copy.toml".to_owned(),
            },
        ],
        ResultData::Fs(vec![
            FsResult {
                id: 1,
                kind: FsActionKind::Create,
                path: 1,
                destination: None,
                state: FsState::Committed,
                digest: Some("b".repeat(64)),
            },
            FsResult {
                id: 2,
                kind: FsActionKind::Copy,
                path: 2,
                destination: Some(3),
                state: FsState::Committed,
                digest: Some("a".repeat(64)),
            },
        ]),
        0,
        None,
    )
    .expect("filesystem response");
    let encoded = response.encode().expect("encode").encode();
    assert_eq!(encoded, FS_RESULT);
    assert_eq!(decode(&encoded).expect("ASON").encode(), encoded);
}
