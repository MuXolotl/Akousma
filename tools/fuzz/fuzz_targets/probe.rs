//! Opens any bytes with the default probe and decodes the default track to
//! the end or to the first error, then finalizes the decoder.

#![no_main]

mod common;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some(mut session) = common::Session::open(data) else {
        return;
    };
    if session.read(usize::MAX) {
        let _ = session.decoder.finalize();
    }
});
