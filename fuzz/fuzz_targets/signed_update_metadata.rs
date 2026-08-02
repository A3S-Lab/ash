#![no_main]

use ash_fuzz::signed_update_metadata::exercise;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| exercise(data));
