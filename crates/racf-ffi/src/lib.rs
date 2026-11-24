//! C ABI for rendering RACF documents as interleaved `f32` PCM.
//!
//! All Rust types are hidden behind the opaque [`RacfEngine`] handle, every
//! entry point catches panics, and errors are reported through integer status
//! codes plus a thread-local message from [`racf_last_error`].
//!
//! The corresponding C99 header is generated into `include/racf.h` at build
//! time by cbindgen.

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{self, AssertUnwindSafe};

use racf_engine::{EngineError, RacfEngine as CoreEngine};

/// Operation completed successfully.
pub const RACF_OK: i32 = 0;
/// A pointer was null, a string was not UTF-8, or an argument was out of range.
pub const RACF_ERR_INVALID_ARGUMENT: i32 = -1;
/// The document file could not be read.
pub const RACF_ERR_IO: i32 = -2;
/// The document was not valid RACF XML.
pub const RACF_ERR_PARSE: i32 = -3;
/// The requested sample rate or channel count is not usable.
pub const RACF_ERR_INVALID_CONFIG: i32 = -4;
/// A Rust panic was caught before it could cross the C ABI boundary.
pub const RACF_ERR_INTERNAL: i32 = -5;

/// Capacity of each metadata string field in RacfInfo, including the terminator.
pub const RACF_METADATA_LEN: usize = 256;

/// Opaque engine handle.
///
/// Create one with `racf_open_file` or `racf_open_str`, and release it with
/// `racf_close`. The handle is a forward-declared type: its fields are private
/// to the library and must never be accessed by callers.
pub struct RacfEngine {
    _private: [u8; 0],
}

/// The actual Rust value behind a [`RacfEngine`] pointer.
struct EngineHandle {
    engine: CoreEngine,
}

/// Static description of a loaded document.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RacfInfo {
    /// Length of the compiled timeline in milliseconds.
    pub duration_ms: u64,
    /// Output sample rate the engine was opened with.
    pub sample_rate: u32,
    /// Number of interleaved channels in each rendered frame.
    pub channels: u16,
    /// NUL-terminated UTF-8 metadata (empty when absent).
    pub title: [c_char; RACF_METADATA_LEN],
    pub artist: [c_char; RACF_METADATA_LEN],
    pub album: [c_char; RACF_METADATA_LEN],
    pub description: [c_char; RACF_METADATA_LEN],
}

thread_local! {
    static LAST_ERROR: RefCell<CString> =
        RefCell::new(CString::new("").expect("an empty C string is always valid"));
}

fn set_error(message: &str) {
    let sanitized = message.replace('\0', " ");
    let value =
        CString::new(sanitized).unwrap_or_else(|_| CString::new("unknown error").expect("valid"));
    LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = value;
    });
}

fn clear_error() {
    set_error("");
}

fn fail(code: i32, message: &str) -> i32 {
    set_error(message);
    code
}

/// Runs `body` so a Rust panic never unwinds into C code.
fn catch_panics<T, F>(fallback: T, body: F) -> T
where
    F: FnOnce() -> T,
{
    match panic::catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            let message = if let Some(text) = payload.downcast_ref::<&str>() {
                (*text).to_string()
            } else if let Some(text) = payload.downcast_ref::<String>() {
                text.clone()
            } else {
                "unknown panic".to_string()
            };
            set_error(&format!("internal error: {message}"));
            fallback
        }
    }
}

#[cfg(unix)]
fn path_from_cstr(value: &CStr) -> std::path::PathBuf {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    std::path::PathBuf::from(OsStr::from_bytes(value.to_bytes()))
}

#[cfg(not(unix))]
fn path_from_cstr(value: &CStr) -> std::path::PathBuf {
    std::path::PathBuf::from(value.to_string_lossy().into_owned())
}

fn write_metadata(dest: &mut [c_char; RACF_METADATA_LEN], value: Option<&str>) {
    dest.fill(0);
    if let Some(value) = value {
        for (slot, byte) in dest
            .iter_mut()
            .zip(value.as_bytes().iter().take(RACF_METADATA_LEN - 1))
        {
            *slot = *byte as c_char;
        }
    }
}

fn take_engine(result: Result<CoreEngine, EngineError>, out_engine: *mut *mut RacfEngine) -> i32 {
    match result {
        Ok(engine) => {
            let handle = Box::new(EngineHandle { engine });
            unsafe {
                *out_engine = Box::into_raw(handle).cast::<RacfEngine>();
            }
            clear_error();
            RACF_OK
        }
        Err(error) => {
            let code = match error {
                EngineError::InvalidSampleRate(_) | EngineError::InvalidChannels(_) => {
                    RACF_ERR_INVALID_CONFIG
                }
                EngineError::Parse(_) => RACF_ERR_PARSE,
            };
            fail(code, &error.to_string())
        }
    }
}

fn validate_output(
    out_engine: *mut *mut RacfEngine,
    sample_rate: u32,
    channels: u16,
) -> Option<i32> {
    if out_engine.is_null() {
        return Some(fail(
            RACF_ERR_INVALID_ARGUMENT,
            "out_engine must not be null",
        ));
    }
    if sample_rate == 0 {
        return Some(fail(
            RACF_ERR_INVALID_ARGUMENT,
            "sample_rate must be greater than zero",
        ));
    }
    if channels == 0 {
        return Some(fail(
            RACF_ERR_INVALID_ARGUMENT,
            "channels must be greater than zero",
        ));
    }
    None
}

/// Opens a RACF document from a UTF-8 file path.
///
/// On success, `*out_engine` receives a handle the caller must pass to
/// `racf_close`. On failure it is left untouched and a status code is returned.
///
/// # Safety
/// `path` must point to a NUL-terminated string. `out_engine` must be a valid,
/// writable pointer to a handle slot.
#[no_mangle]
pub unsafe extern "C" fn racf_open_file(
    path: *const c_char,
    sample_rate: u32,
    channels: u16,
    out_engine: *mut *mut RacfEngine,
) -> i32 {
    catch_panics(RACF_ERR_INTERNAL, || {
        if let Some(code) = validate_output(out_engine, sample_rate, channels) {
            return code;
        }
        if path.is_null() {
            return fail(RACF_ERR_INVALID_ARGUMENT, "path must not be null");
        }

        let path = path_from_cstr(CStr::from_ptr(path));
        let xml = match std::fs::read_to_string(&path) {
            Ok(xml) => xml,
            Err(error) => {
                return fail(
                    RACF_ERR_IO,
                    &format!("cannot read {}: {error}", path.display()),
                );
            }
        };

        take_engine(
            CoreEngine::from_xml(&xml, sample_rate, channels),
            out_engine,
        )
    })
}

/// Opens a RACF document from a NUL-terminated UTF-8 XML string.
///
/// # Safety
/// `xml` must point to a NUL-terminated string. `out_engine` must be a valid,
/// writable pointer to a handle slot.
#[no_mangle]
pub unsafe extern "C" fn racf_open_str(
    xml: *const c_char,
    sample_rate: u32,
    channels: u16,
    out_engine: *mut *mut RacfEngine,
) -> i32 {
    catch_panics(RACF_ERR_INTERNAL, || {
        if let Some(code) = validate_output(out_engine, sample_rate, channels) {
            return code;
        }
        if xml.is_null() {
            return fail(RACF_ERR_INVALID_ARGUMENT, "xml must not be null");
        }

        let xml = match CStr::from_ptr(xml).to_str() {
            Ok(xml) => xml,
            Err(error) => {
                return fail(
                    RACF_ERR_INVALID_ARGUMENT,
                    &format!("invalid UTF-8: {error}"),
                )
            }
        };

        take_engine(CoreEngine::from_xml(xml, sample_rate, channels), out_engine)
    })
}

/// Copies static document information into `*out_info`.
///
/// # Safety
/// `engine` must be null or a live handle returned by `racf_open_*`.
/// `out_info` must point to a writable `RacfInfo`.
#[no_mangle]
pub unsafe extern "C" fn racf_get_info(engine: *const RacfEngine, out_info: *mut RacfInfo) -> i32 {
    catch_panics(RACF_ERR_INTERNAL, || {
        if engine.is_null() || out_info.is_null() {
            return fail(
                RACF_ERR_INVALID_ARGUMENT,
                "engine and out_info must not be null",
            );
        }

        let handle = &*(engine.cast::<EngineHandle>());
        let source = &handle.engine;
        let metadata = &source.document().metadata;
        let info = RacfInfo {
            duration_ms: source.duration_ms(),
            sample_rate: source.sample_rate(),
            channels: source.channels() as u16,
            title: {
                let mut field = [0; RACF_METADATA_LEN];
                write_metadata(&mut field, metadata.title.as_deref());
                field
            },
            artist: {
                let mut field = [0; RACF_METADATA_LEN];
                write_metadata(&mut field, metadata.artist.as_deref());
                field
            },
            album: {
                let mut field = [0; RACF_METADATA_LEN];
                write_metadata(&mut field, metadata.album.as_deref());
                field
            },
            description: {
                let mut field = [0; RACF_METADATA_LEN];
                write_metadata(&mut field, metadata.description.as_deref());
                field
            },
        };

        *out_info = info;
        clear_error();
        RACF_OK
    })
}

/// Renders interleaved `f32` frames into `out_buffer`.
///
/// Returns the number of frames written, `0` at end of stream, or a negative
/// status code on error. When fewer frames are returned than requested, the
/// unused part of the buffer is zeroed.
///
/// # Safety
/// `engine` must be null or a live handle returned by `racf_open_*`.
/// `out_buffer` must be writable for at least `max_frames * channels` floats,
/// where `channels` is the value reported by `racf_get_info`.
#[no_mangle]
pub unsafe extern "C" fn racf_read_f32(
    engine: *mut RacfEngine,
    out_buffer: *mut f32,
    max_frames: usize,
) -> isize {
    catch_panics(RACF_ERR_INTERNAL as isize, || {
        if engine.is_null() || out_buffer.is_null() {
            fail(
                RACF_ERR_INVALID_ARGUMENT,
                "engine and out_buffer must not be null",
            );
            return RACF_ERR_INVALID_ARGUMENT as isize;
        }
        if max_frames == 0 {
            return 0;
        }

        let handle = &mut *(engine.cast::<EngineHandle>());
        let Some(samples) = max_frames.checked_mul(handle.engine.channels()) else {
            fail(
                RACF_ERR_INVALID_ARGUMENT,
                "max_frames is too large for this buffer",
            );
            return RACF_ERR_INVALID_ARGUMENT as isize;
        };

        let buffer = std::slice::from_raw_parts_mut(out_buffer, samples);
        let frames = handle.engine.render_f32(buffer);
        clear_error();
        frames as isize
    })
}

/// Seeks to a position in milliseconds.
///
/// # Safety
/// `engine` must be null or a live handle returned by `racf_open_*`.
#[no_mangle]
pub unsafe extern "C" fn racf_seek(engine: *mut RacfEngine, timestamp_ms: u64) -> i32 {
    catch_panics(RACF_ERR_INTERNAL, || {
        if engine.is_null() {
            return fail(RACF_ERR_INVALID_ARGUMENT, "engine must not be null");
        }

        let handle = &mut *(engine.cast::<EngineHandle>());
        handle.engine.seek(timestamp_ms);
        clear_error();
        RACF_OK
    })
}

/// Returns a pointer to the last error message on the calling thread.
///
/// The string is valid until the next API call on the same thread sets a new
/// error. It is always NUL-terminated and empty after a successful call.
#[no_mangle]
pub extern "C" fn racf_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| slot.borrow().as_ptr())
}

/// Releases an engine handle. Passing null is a no-op.
///
/// # Safety
/// `engine` must be null or a live handle from `racf_open_*` that has not
/// already been closed. The pointer is invalid after this call.
#[no_mangle]
pub unsafe extern "C" fn racf_close(engine: *mut RacfEngine) {
    catch_panics((), || {
        if !engine.is_null() {
            drop(Box::from_raw(engine.cast::<EngineHandle>()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = r#"
        <racf tempo="60000" channelLayout="stereo">
            <metadata title="Test" artist="Andres" />
            <score>
                <track instrument="Lead">
                    <note pitch="C4" duration="1/4" />
                </track>
            </score>
        </racf>
    "#;

    fn cstring(value: &str) -> CString {
        CString::new(value).unwrap()
    }

    fn error_message() -> String {
        unsafe { CStr::from_ptr(racf_last_error()) }
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn open_info_read_seek_and_close() {
        let xml = cstring(SIMPLE);
        let mut engine: *mut RacfEngine = std::ptr::null_mut();
        assert_eq!(
            unsafe { racf_open_str(xml.as_ptr(), 8_000, 2, &mut engine) },
            RACF_OK
        );
        assert!(!engine.is_null());
        assert!(error_message().is_empty());

        let mut info = std::mem::MaybeUninit::<RacfInfo>::uninit();
        assert_eq!(unsafe { racf_get_info(engine, info.as_mut_ptr()) }, RACF_OK);
        let info = unsafe { info.assume_init() };
        assert_eq!(info.duration_ms, 1);
        assert_eq!(info.sample_rate, 8_000);
        assert_eq!(info.channels, 2);
        assert_eq!(
            unsafe { CStr::from_ptr(info.title.as_ptr()) }
                .to_str()
                .unwrap(),
            "Test"
        );
        assert_eq!(
            unsafe { CStr::from_ptr(info.artist.as_ptr()) }
                .to_str()
                .unwrap(),
            "Andres"
        );
        assert_eq!(
            unsafe { CStr::from_ptr(info.album.as_ptr()) }
                .to_str()
                .unwrap(),
            ""
        );

        let mut buffer = vec![0.0f32; 2 * 16];
        assert_eq!(
            unsafe { racf_read_f32(engine, buffer.as_mut_ptr(), 16) },
            16
        );
        assert!(buffer.iter().any(|sample| *sample != 0.0));

        assert_eq!(unsafe { racf_seek(engine, 1) }, RACF_OK);
        assert_eq!(
            unsafe { racf_read_f32(engine, buffer.as_mut_ptr(), 16) },
            16
        );

        unsafe { racf_close(engine) };
    }

    #[test]
    fn reports_errors_and_rejects_null_arguments() {
        let mut engine: *mut RacfEngine = std::ptr::null_mut();
        let bad_xml = cstring("<not-racf />");
        assert_eq!(
            unsafe { racf_open_str(bad_xml.as_ptr(), 44_100, 2, &mut engine) },
            RACF_ERR_PARSE
        );
        assert!(error_message().contains("expected <racf>"));

        assert_eq!(
            unsafe { racf_open_str(std::ptr::null(), 44_100, 2, &mut engine) },
            RACF_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { racf_read_f32(std::ptr::null_mut(), std::ptr::null_mut(), 8) },
            RACF_ERR_INVALID_ARGUMENT as isize
        );
        assert_eq!(
            unsafe { racf_open_str(bad_xml.as_ptr(), 0, 2, &mut engine) },
            RACF_ERR_INVALID_ARGUMENT
        );

        unsafe { racf_close(std::ptr::null_mut()) };
    }

    #[test]
    fn opens_documents_from_files() {
        let path_buf = std::env::temp_dir().join(format!("racf-ffi-{}.racf", std::process::id()));
        std::fs::write(&path_buf, SIMPLE).unwrap();
        let path = cstring(path_buf.to_str().unwrap());

        let mut engine: *mut RacfEngine = std::ptr::null_mut();
        assert_eq!(
            unsafe { racf_open_file(path.as_ptr(), 44_100, 2, &mut engine) },
            RACF_OK
        );

        let mut info = std::mem::MaybeUninit::<RacfInfo>::uninit();
        assert_eq!(unsafe { racf_get_info(engine, info.as_mut_ptr()) }, RACF_OK);
        assert_eq!(unsafe { info.assume_init() }.sample_rate, 44_100);
        unsafe { racf_close(engine) };
        std::fs::remove_file(path_buf).unwrap();

        let missing = cstring("/this/path/does/not/exist.racf");
        assert_eq!(
            unsafe { racf_open_file(missing.as_ptr(), 44_100, 2, &mut engine) },
            RACF_ERR_IO
        );
        assert!(error_message().contains("cannot read"));
    }

    #[test]
    fn reports_end_of_stream() {
        let xml = cstring(SIMPLE);
        let mut engine: *mut RacfEngine = std::ptr::null_mut();
        assert_eq!(
            unsafe { racf_open_str(xml.as_ptr(), 1_000, 1, &mut engine) },
            RACF_OK
        );

        let mut buffer = [0.0f32; 32];
        let mut total = 0;
        while unsafe { racf_read_f32(engine, buffer.as_mut_ptr(), buffer.len()) } > 0 {
            total += buffer.len();
            assert!(total < 10_000);
        }

        unsafe { racf_close(engine) };
    }
}
