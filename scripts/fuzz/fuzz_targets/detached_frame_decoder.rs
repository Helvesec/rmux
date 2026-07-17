#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    rmux_proto::fuzz_detached_frame_decoder(data);
});
