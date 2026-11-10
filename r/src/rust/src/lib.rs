use arrow_array::ffi_stream::FFI_ArrowArrayStream;
use extendr_api::prelude::*;
use extendr_ffi::{R_ClearExternalPtr, R_ExternalPtrAddr, R_MakeExternalPtr, SEXP};

mod bridge;

/// The core version, then the Athena and S3 SDK versions.
/// @noRd
#[extendr]
fn core_versions() -> Vec<String> {
    ckdata::versions()
}

extern "C" fn finalize_stream(value: SEXP) {
    // SAFETY: this finalizer is registered only on pointers created below from a Box.
    // Clear first so finalization can never release the same stream twice. Dropping the
    // Arrow stream calls its release callback unless a consumer already moved it.
    unsafe {
        let stream = R_ExternalPtrAddr(value).cast::<FFI_ArrowArrayStream>();
        R_ClearExternalPtr(value);
        if !stream.is_null() {
            drop(Box::from_raw(stream));
        }
    }
}

fn owned_stream(
    reader: impl arrow_array::RecordBatchReader + Send + 'static,
) -> extendr_api::Result<Robj> {
    let stream = Box::new(FFI_ArrowArrayStream::new(Box::new(reader)));
    // SAFETY: Rust supplies the allocation; no caller-provided address is accepted.
    // The external pointer owns the Box, and nanoarrow consumes the standard Arrow C
    // stream. Its tag and protected object are NULL as required by nanoarrow/r.h.
    unsafe {
        let nil = ().into_robj();
        let mut value = Robj::from_sexp(R_MakeExternalPtr(
            Box::into_raw(stream).cast(),
            nil.get(),
            nil.get(),
        ));
        value.register_c_finalizer(Some(finalize_stream));
        value.set_class(["nanoarrow_array_stream"])?;
        Ok(value)
    }
}

extendr_module! {
    mod ckdata;
    fn core_versions;
    use bridge;
}
