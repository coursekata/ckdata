use arrow_array::ffi_stream::FFI_ArrowArrayStream;
use extendr_api::prelude::*;

/// The SDK clients the core links, configured offline.
/// @export
#[extendr]
fn ck_versions() -> Vec<String> {
    ckcore::versions()
}

/// Fill an empty nanoarrow stream, given its address, with the core's literal probe.
/// @export
#[extendr]
fn ck_probe_into(address: &str) {
    let ptr = address.parse::<usize>().expect("a stream address") as *mut FFI_ArrowArrayStream;
    let stream = FFI_ArrowArrayStream::new(ckcore::probe_reader());
    // The stream nanoarrow allocated is empty, so there is nothing to release before writing.
    unsafe { std::ptr::write(ptr, stream) };
}

extendr_module! {
    mod ckspike;
    fn ck_versions;
    fn ck_probe_into;
}
