// SPDX-License-Identifier: GPL-3.0-or-later
//! IPC message parser: framed reads of every message type, plus raw
//! payload decoding. Must never panic or allocate beyond the frame limit.
#![no_main]

use explorer_index::protocol::{
    decode, read_frame, HelperRequest, HelperResponse, Request, Response, MAX_HELPER_FRAME,
    MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = decode::<Request>(data);
    let _ = decode::<Response>(data);
    let _ = decode::<HelperRequest>(data);
    let _ = decode::<HelperResponse>(data);
    let mut r = data;
    while read_frame::<Request>(&mut r, MAX_REQUEST_FRAME).is_ok() {}
    let mut r = data;
    while read_frame::<Response>(&mut r, MAX_RESPONSE_FRAME).is_ok() {}
    let mut r = data;
    while read_frame::<HelperResponse>(&mut r, MAX_HELPER_FRAME).is_ok() {}
});
