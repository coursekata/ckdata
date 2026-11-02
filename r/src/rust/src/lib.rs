use arrow_array::ffi_stream::FFI_ArrowArrayStream;
use extendr_api::prelude::*;

/// The core's version, then the version of each AWS SDK crate it links.
/// @noRd
#[extendr]
fn core_versions() -> Vec<String> {
    ckdata::versions()
}

/// Fill an empty nanoarrow stream, given its address, with the core's literal probe.
/// @noRd
#[extendr]
fn arrow_probe_into(address: &str) -> extendr_api::Result<()> {
    let address: usize = address
        .parse()
        .map_err(|_| Error::Other(format!("not a stream address: {address}")))?;
    let reader = ckdata::arrow_probe().map_err(|e| Error::Other(e.to_string()))?;
    let stream = FFI_ArrowArrayStream::new(reader);
    // SAFETY: the address is a stream nanoarrow allocated for this call and left empty, so it is
    // valid to write and holds nothing to release first.
    unsafe { std::ptr::write(address as *mut FFI_ArrowArrayStream, stream) };
    Ok(())
}

extendr_module! {
    mod ckdata;
    fn core_versions;
    fn arrow_probe_into;
}
