#![no_main]

use std::sync::LazyLock;

use ash_update::{TrustStore, verify_release};
use libfuzzer_sys::fuzz_target;

static TRUST: LazyLock<TrustStore> = LazyLock::new(|| {
    TrustStore::parse("fuzz-1=d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
        .expect("fixed fuzz trust root")
});

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let payload = &data[2..];
    let split = usize::from(u16::from_be_bytes([data[0], data[1]])) % (payload.len() + 1);
    let (manifest, signature) = payload.split_at(split);
    let _ = verify_release(
        manifest,
        signature,
        &TRUST,
        "0.1.0",
        "0.1.0",
        (1, 0),
        (1, 0),
        "x86_64-unknown-linux-musl",
        0,
        None,
    );
});
