use super::{
    Arguments, BatchArgs, BatchNode, Budget, CancelArgs, EXEC_CLEAR_ENVIRONMENT, ExecArgs,
    FsAction, FsActionKind, FsArgs, InputSource, LIST_INCLUDE_HIDDEN, ListArgs, PATCH_COLUMNS,
    PatchArgs, PatchContent, PatchEdit, READ_COLUMNS, REF_CASE_INSENSITIVE, REF_REGEX, ReadArgs,
    ReadMode, RefArgs, RefMode, Request, RequestError, SEARCH_CASE_INSENSITIVE, SEARCH_REGEX,
    SNAPSHOT_INCLUDE_HIDDEN, SearchArgs, SnapshotArgs, SnapshotMode,
};
use crate::Operation;
use crate::ason::{Atom, Cell, Document, Field, Key, Record, Value, decode};

const SEARCH_REQUEST: &str = include_str!("../../../../spec/fixtures/ason/search-request.ason");
const EXEC_REQUEST: &str = include_str!("../../../../spec/fixtures/ason/exec-request.ason");
const READ_REQUEST: &str = include_str!("../../../../spec/fixtures/ason/read-request.ason");
const LIST_REQUEST: &str = include_str!("../../../../spec/fixtures/ason/list-request.ason");
const CANCEL_REQUEST: &str = include_str!("../../../../spec/fixtures/ason/cancel-request.ason");
const REF_REQUEST: &str = include_str!("../../../../spec/fixtures/ason/ref-request.ason");
const PATCH_REQUEST: &str = include_str!("../../../../spec/fixtures/ason/patch-request.ason");
const FS_REQUEST: &str = include_str!("../../../../spec/fixtures/ason/fs-request.ason");
const SNAPSHOT_REQUEST: &str = include_str!("../../../../spec/fixtures/ason/snapshot-request.ason");
const BATCH_REQUEST: &str = include_str!("../../../../spec/fixtures/ason/batch-request.ason");

fn budget() -> Budget {
    Budget::new(256, 64, 30_000).expect("budget")
}

#[test]
fn specification_search_fixture_decodes_and_reencodes_exactly() {
    let request = Request::decode(&decode(SEARCH_REQUEST).expect("ASON")).expect("schema");
    assert_eq!(request.id(), 17);
    assert_eq!(request.operation(), Operation::Search);
    let Arguments::Search(arguments) = request.arguments() else {
        panic!("search arguments expected");
    };
    assert_eq!(arguments.query(), "TODO");
    assert_eq!(arguments.paths(), &["src"]);
    assert_eq!(request.encode().expect("encode").encode(), SEARCH_REQUEST);
}

#[test]
fn all_core_request_fixtures_are_canonical_typed_messages() {
    let expected = [
        (EXEC_REQUEST, Operation::Exec),
        (READ_REQUEST, Operation::Read),
        (LIST_REQUEST, Operation::List),
        (SEARCH_REQUEST, Operation::Search),
        (PATCH_REQUEST, Operation::Patch),
        (FS_REQUEST, Operation::Fs),
        (BATCH_REQUEST, Operation::Batch),
        (SNAPSHOT_REQUEST, Operation::Snapshot),
        (REF_REQUEST, Operation::Ref),
        (CANCEL_REQUEST, Operation::Cancel),
    ];
    for (fixture, operation) in expected {
        let request = Request::decode(&decode(fixture).expect("ASON")).expect("schema");
        assert_eq!(request.operation(), operation);
        assert_eq!(request.encode().expect("encode").encode(), fixture);
    }
}

#[test]
fn filesystem_fixture_is_typed_and_transactions_reject_overlapping_paths() {
    let request = Request::decode(&decode(FS_REQUEST).expect("ASON")).expect("filesystem schema");
    let Arguments::Fs(filesystem) = request.arguments() else {
        panic!("filesystem arguments expected");
    };
    assert_eq!(filesystem.actions().len(), 2);
    assert_eq!(filesystem.actions()[0].kind(), FsActionKind::Create);
    assert_eq!(
        filesystem.actions()[1].destination(),
        Some("Cargo.copy.toml")
    );
    assert_eq!(request.encode().expect("encode").encode(), FS_REQUEST);

    let digest = "a".repeat(64);
    let overlapping = vec![
        FsAction::new(
            1,
            FsActionKind::Remove,
            "same.txt",
            None,
            Some(digest.clone()),
            None,
        )
        .expect("remove"),
        FsAction::new(
            2,
            FsActionKind::Copy,
            "source.txt",
            Some("same.txt".to_owned()),
            Some(digest),
            None,
        )
        .expect("copy"),
    ];
    assert_eq!(
        FsArgs::new(overlapping),
        Err(RequestError::UnexpectedValue("p"))
    );
    assert!(FsAction::new(3, FsActionKind::Create, "new", None, None, None).is_err());
}

#[test]
fn batch_fixture_round_trips_nested_canonical_arguments() {
    let request = Request::decode(&decode(BATCH_REQUEST).expect("ASON")).expect("batch schema");
    let Arguments::Batch(batch) = request.arguments() else {
        panic!("batch arguments expected");
    };
    assert_eq!(batch.nodes().len(), 2);
    assert_eq!(batch.nodes()[0].id(), 1);
    assert_eq!(batch.nodes()[1].dependencies(), &[1]);
    assert!(matches!(batch.nodes()[0].arguments(), Arguments::Search(_)));
    assert!(matches!(batch.nodes()[1].arguments(), Arguments::Read(_)));
    assert_eq!(request.encode().expect("encode").encode(), BATCH_REQUEST);
}

#[test]
fn batch_graph_rejects_cycles_unknown_edges_and_insufficient_budgets() {
    let search =
        || Arguments::Search(SearchArgs::new("needle", vec![".".to_owned()], 0).expect("search"));
    let cycle = vec![
        BatchNode::new(1, vec![2], search()).expect("node"),
        BatchNode::new(2, vec![1], search()).expect("node"),
    ];
    assert_eq!(
        BatchArgs::new(cycle),
        Err(RequestError::UnexpectedValue("d"))
    );
    assert_eq!(
        BatchArgs::new(vec![
            BatchNode::new(1, vec![], search()).expect("node"),
            BatchNode::new(2, vec![9], search()).expect("node"),
        ]),
        Err(RequestError::UnexpectedValue("d"))
    );
    let batch = BatchArgs::new(vec![
        BatchNode::new(1, vec![], search()).expect("node"),
        BatchNode::new(2, vec![1], search()).expect("node"),
    ])
    .expect("batch");
    assert_eq!(
        Request::new(
            90,
            Arguments::Batch(batch),
            Budget::new(1, 1, 30_000).expect("budget"),
        ),
        Err(RequestError::InvalidLimit("u"))
    );
}

#[test]
fn every_core_argument_schema_round_trips() {
    let requests = [
        Request::new(
            1,
            Arguments::Exec(
                ExecArgs::new(
                    "cargo",
                    vec!["test".to_owned(), "--locked".to_owned()],
                    ".",
                    vec!["RUST_BACKTRACE=1".to_owned(), "-SECRET".to_owned()],
                    InputSource::Reference(9),
                    EXEC_CLEAR_ENVIRONMENT,
                )
                .expect("exec"),
            ),
            budget(),
        )
        .expect("request"),
        Request::new(
            2,
            Arguments::Read(
                ReadArgs::new(vec!["src/lib.rs".to_owned()], ReadMode::Lines, 10, 20)
                    .expect("read"),
            ),
            budget(),
        )
        .expect("request"),
        Request::new(
            3,
            Arguments::List(
                ListArgs::new(vec!["src".to_owned()], 4, LIST_INCLUDE_HIDDEN).expect("list"),
            ),
            budget(),
        )
        .expect("request"),
        Request::new(
            4,
            Arguments::Search(
                SearchArgs::new(
                    "error|warning",
                    vec!["src".to_owned()],
                    SEARCH_REGEX | SEARCH_CASE_INSENSITIVE,
                )
                .expect("search"),
            ),
            budget(),
        )
        .expect("request"),
        Request::new(
            5,
            Arguments::Patch(
                PatchArgs::new(
                    vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()],
                    vec!["a".repeat(64), "b".repeat(64)],
                    vec![
                        PatchEdit::new(0, 4, 3, PatchContent::Inline("pub".to_owned()))
                            .expect("edit"),
                        PatchEdit::new(1, 0, 0, PatchContent::Reference(11)).expect("edit"),
                    ],
                    0,
                )
                .expect("patch"),
            ),
            budget(),
        )
        .expect("request"),
        Request::new(
            6,
            Arguments::Snapshot(
                SnapshotArgs::new(
                    vec![".".to_owned()],
                    64,
                    SnapshotMode::Delta,
                    Some(12),
                    SNAPSHOT_INCLUDE_HIDDEN,
                )
                .expect("snapshot"),
            ),
            budget(),
        )
        .expect("request"),
        Request::new(
            7,
            Arguments::Ref(
                RefArgs::new(
                    9,
                    RefMode::Search,
                    0,
                    1_048_576,
                    Some("error|warning".to_owned()),
                    REF_REGEX | REF_CASE_INSENSITIVE,
                )
                .expect("reference"),
            ),
            budget(),
        )
        .expect("request"),
        Request::new(
            8,
            Arguments::Cancel(CancelArgs::new(4).expect("cancel")),
            budget(),
        )
        .expect("request"),
    ];

    for request in requests {
        let encoded = request.encode().expect("encode").encode();
        let decoded = Request::decode(&decode(&encoded).expect("ASON")).expect("schema");
        assert_eq!(decoded, request);
    }
}

#[test]
fn patch_schema_requires_canonical_paths_digests_and_non_overlapping_edits() {
    let digest = "a".repeat(64);
    assert_eq!(
        PatchArgs::new(
            vec!["b".to_owned(), "a".to_owned()],
            vec![digest.clone(), digest.clone()],
            vec![
                PatchEdit::new(0, 0, 1, PatchContent::Inline(String::new())).expect("edit"),
                PatchEdit::new(1, 0, 1, PatchContent::Inline(String::new())).expect("edit"),
            ],
            0,
        ),
        Err(RequestError::UnexpectedValue("p"))
    );
    assert_eq!(
        PatchArgs::new(
            vec!["a".to_owned()],
            vec![digest],
            vec![
                PatchEdit::new(0, 1, 2, PatchContent::Inline("x".to_owned())).expect("edit"),
                PatchEdit::new(0, 2, 1, PatchContent::Inline("y".to_owned())).expect("edit"),
            ],
            0,
        ),
        Err(RequestError::UnexpectedValue("o"))
    );

    let record = Record::new(
        PATCH_COLUMNS
            .iter()
            .map(|key| Key::new(*key).expect("key"))
            .collect(),
        vec![
            Cell::Vector(vec![Atom::text("a")]),
            Cell::Vector(vec![Atom::text("not-a-digest")]),
            Cell::Vector(vec![Atom::text("0")]),
            Cell::Vector(vec![Atom::text("0")]),
            Cell::Vector(vec![Atom::text("0")]),
            Cell::Vector(vec![Atom::text("x")]),
            Cell::Atom(Atom::text("0")),
        ],
    )
    .expect("record");
    assert_eq!(
        PatchArgs::decode(&record),
        Err(RequestError::InvalidText("h"))
    );
}

#[test]
fn strict_envelope_budget_and_operation_validation_rejects_ambiguity() {
    for input in [
        "t:1\ni:17\no:g\na{q,p,f}:\nTODO,[src],0\n",
        "t:1\ni:0\no:g\na{q,p,f}:\nTODO,[src],0\nu{tok,rec,ms}:\n256,64,30000\n",
        "t:1\ni:17\no:q\na{q,p,f}:\nTODO,[src],0\nu{tok,rec,ms}:\n256,64,30000\n",
        "t:1\ni:17\no:g\na{q,p,f}:\nTODO,[src],8\nu{tok,rec,ms}:\n256,64,30000\n",
        "t:1\ni:17\no:g\na{q,p,f}:\nTODO,[],0\nu{tok,rec,ms}:\n256,64,30000\n",
    ] {
        assert!(
            Request::decode(&decode(input).expect("valid ASON syntax")).is_err(),
            "unexpected valid request: {input:?}"
        );
    }
}

#[test]
fn line_reads_are_one_based_and_environment_deltas_are_unique() {
    assert_eq!(
        ReadArgs::new(vec!["a".to_owned()], ReadMode::Lines, 0, 1),
        Err(RequestError::InvalidLimit("o"))
    );
    assert!(matches!(
        ExecArgs::new(
            "tool",
            vec![],
            ".",
            vec!["A=1".to_owned(), "-A".to_owned()],
            InputSource::None,
            0,
        ),
        Err(RequestError::InvalidText("e"))
    ));
    assert_eq!(CancelArgs::new(0), Err(RequestError::InvalidUnsigned("i")));
    assert_eq!(
        RefArgs::new(0, RefMode::Bytes, 0, 1, None, 0),
        Err(RequestError::InvalidUnsigned("r"))
    );
    assert_eq!(
        RefArgs::new(1, RefMode::Release, 0, 1, None, 0),
        Err(RequestError::UnexpectedValue("m"))
    );
    assert_eq!(
        SnapshotArgs::new(vec![".".to_owned()], 64, SnapshotMode::Delta, None, 0),
        Err(RequestError::UnexpectedValue("r"))
    );
    assert_eq!(
        Request::new(
            7,
            Arguments::Cancel(CancelArgs::new(7).expect("cancel")),
            budget(),
        ),
        Err(RequestError::UnexpectedValue("i"))
    );
}

#[test]
fn wrong_argument_column_order_is_rejected_before_dispatch() {
    let document = Document::new(vec![
        Field::new(Key::new("t").expect("key"), Value::Scalar(Atom::text("1"))),
        Field::new(Key::new("i").expect("key"), Value::Scalar(Atom::text("1"))),
        Field::new(Key::new("o").expect("key"), Value::Scalar(Atom::text("r"))),
        Field::new(
            Key::new("a").expect("key"),
            Value::Record(
                Record::new(
                    READ_COLUMNS
                        .iter()
                        .rev()
                        .map(|key| Key::new(*key).expect("key"))
                        .collect(),
                    vec![
                        Cell::Atom(Atom::text("1")),
                        Cell::Atom(Atom::text("0")),
                        Cell::Atom(Atom::text("0")),
                        Cell::Vector(vec![Atom::text("a")]),
                    ],
                )
                .expect("record"),
            ),
        ),
        Field::new(
            Key::new("u").expect("key"),
            Value::Record(
                Record::new(
                    ["tok", "rec", "ms"]
                        .into_iter()
                        .map(|key| Key::new(key).expect("key"))
                        .collect(),
                    vec![
                        Cell::Atom(Atom::text("1")),
                        Cell::Atom(Atom::text("1")),
                        Cell::Atom(Atom::text("1")),
                    ],
                )
                .expect("record"),
            ),
        ),
    ])
    .expect("document");
    assert_eq!(Request::decode(&document), Err(RequestError::Columns));
    assert_eq!(Request::id_hint(&document), Some(1));
}
