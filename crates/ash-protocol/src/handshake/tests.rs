use super::{
    HandshakePreferences, HandshakeRequest, HandshakeResponse, SchemaError, ServerHandshake,
};
use crate::ason::decode;
use crate::{ALL_CAPABILITY_MASK, ALL_OPERATION_MASK};

const HANDSHAKE_REQUEST: &str =
    include_str!("../../../../spec/fixtures/ason/handshake-request.ason");
const HANDSHAKE_RESPONSE: &str =
    include_str!("../../../../spec/fixtures/ason/handshake-response.ason");

#[test]
fn specification_fixtures_match_the_typed_schema() {
    let request = HandshakeRequest::decode(&decode(HANDSHAKE_REQUEST).expect("ASON request"))
        .expect("request schema");
    assert_eq!(request.request_id(), 7);
    assert_eq!(request.nonce(), "nonce-7");
    assert_eq!(request.preferences().capability_mask, ALL_CAPABILITY_MASK);

    let response = HandshakeResponse::decode(&decode(HANDSHAKE_RESPONSE).expect("ASON response"))
        .expect("response schema");
    assert_eq!(response.session_id(), 1);
    assert_eq!(response.nonce(), "nonce-7");
    assert_eq!(response.capability_mask(), ALL_CAPABILITY_MASK);
}

#[test]
fn request_and_response_round_trip_through_ason() {
    let request = HandshakeRequest::new(
        7,
        ".",
        "nonce-7",
        HandshakePreferences {
            operation_mask: 0x3ff,
            capability_mask: 3,
            ..HandshakePreferences::default()
        },
    )
    .expect("valid request");
    let encoded = request.encode().expect("encode request").encode();
    let decoded =
        HandshakeRequest::decode(&decode(&encoded).expect("decode syntax")).expect("decode schema");
    assert_eq!(decoded, request);

    let server = ServerHandshake {
        operation_mask: 0b101,
        capability_mask: 1,
        os: "test-os".to_owned(),
        arch: "test-arch".to_owned(),
        ..ServerHandshake::default()
    };
    let response = server.negotiate(&request, 42).expect("negotiate");
    let encoded = response.encode().expect("encode response").encode();
    let decoded = HandshakeResponse::decode(&decode(&encoded).expect("decode syntax"))
        .expect("decode schema");
    assert_eq!(decoded, response);
    assert_eq!(decoded.operation_mask(), 0b101);
    assert_eq!(decoded.capability_mask(), 1);
    assert_eq!(decoded.session_id(), 42);
    assert_eq!(decoded.nonce(), "nonce-7");
}

#[test]
fn version_and_capability_negotiation_never_overclaims() {
    let request = HandshakeRequest::new(
        1,
        ".",
        "n",
        HandshakePreferences {
            max_frame_bytes: 64 * 1024,
            output_bytes: 4096,
            operation_mask: u64::MAX,
            capability_mask: u64::MAX,
            ..HandshakePreferences::default()
        },
    )
    .expect("valid request");
    let response = ServerHandshake::default()
        .negotiate(&request, 1)
        .expect("negotiate");
    assert_eq!(response.frame_bytes(), 64 * 1024);
    assert_eq!(response.operation_mask(), 0);
}

#[test]
fn schema_rejects_invalid_ranges_limits_and_response_metadata() {
    let invalid_range = HandshakeRequest::new(
        1,
        ".",
        "n",
        HandshakePreferences {
            ash_minor_low: 1,
            ash_minor_high: 0,
            ..HandshakePreferences::default()
        },
    );
    assert_eq!(invalid_range, Err(SchemaError::InvalidRange));

    let zero_frame = HANDSHAKE_REQUEST.replace("1048576,65536", "0,65536");
    assert_eq!(
        HandshakeRequest::decode(&decode(&zero_frame).expect("ASON request")),
        Err(SchemaError::InvalidLimit("frm"))
    );

    let zero_session = HANDSHAKE_RESPONSE.replace(",1,nonce-7", ",0,nonce-7");
    assert_eq!(
        HandshakeResponse::decode(&decode(&zero_session).expect("ASON response")),
        Err(SchemaError::UnexpectedValue("sid"))
    );
}

#[test]
fn server_masks_operation_bits_unknown_to_this_minor_version() {
    let request = HandshakeRequest::new(
        1,
        ".",
        "n",
        HandshakePreferences {
            operation_mask: u64::MAX,
            capability_mask: u64::MAX,
            ..HandshakePreferences::default()
        },
    )
    .expect("valid request");
    let response = ServerHandshake {
        operation_mask: u64::MAX,
        capability_mask: u64::MAX,
        ..ServerHandshake::default()
    }
    .negotiate(&request, 1)
    .expect("negotiate");
    assert_eq!(response.operation_mask(), ALL_OPERATION_MASK);
    assert_eq!(response.capability_mask(), ALL_CAPABILITY_MASK);
}
