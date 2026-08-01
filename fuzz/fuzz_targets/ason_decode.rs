#![no_main]

use ash_protocol::ason::decode;
use ash_protocol::handshake::{HandshakeRequest, HandshakeResponse};
use ash_protocol::request::Request;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(document) = decode(text) else {
        return;
    };
    let canonical = document.encode();
    assert_eq!(decode(&canonical).expect("canonical document"), document);
    let _ = HandshakeRequest::decode(&document);
    let _ = HandshakeResponse::decode(&document);
    let _ = Request::decode(&document);
});
