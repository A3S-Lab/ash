#![no_main]

use std::future::Future;
use std::io::Cursor;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use ash_protocol::ason::Limits;
use ash_protocol::frame::{FrameCodec, HARD_MAX_FRAME_BYTES};
use ash_protocol::handshake::{HandshakeRequest, HandshakeResponse};
use ash_protocol::request::Request;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let payload = &data[3..];
    let max_payload = 1 + usize::from(data[1]) * (HARD_MAX_FRAME_BYTES / 256);
    let declared = match data[0] % 5 {
        0 => payload.len(),
        1 => 0,
        2 => payload.len().saturating_add(usize::from(data[2]) + 1),
        3 => payload.len().saturating_sub(usize::from(data[2]) + 1),
        _ => usize::from(u16::from_be_bytes([data[1], data[2]])),
    };
    let declared = u32::try_from(declared).unwrap_or(u32::MAX);
    let mut framed = Vec::with_capacity(4 + payload.len());
    framed.extend_from_slice(&declared.to_be_bytes());
    framed.extend_from_slice(payload);

    let codec = FrameCodec::new(max_payload).expect("bounded fuzz frame limit");
    let mut reader = Cursor::new(&framed);
    let result = poll_ready(codec.read_document(&mut reader, &Limits::default()));
    let Ok(Some(document)) = result else {
        return;
    };

    let consumed = usize::try_from(reader.position()).expect("cursor position");
    assert_eq!(consumed, 4 + declared as usize);
    assert_eq!(document.encode().as_bytes(), &framed[4..consumed]);
    let _ = HandshakeRequest::decode(&document);
    let _ = HandshakeResponse::decode(&document);
    let _ = Request::decode(&document);
});

fn poll_ready<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("in-memory frame reader unexpectedly yielded"),
    }
}
