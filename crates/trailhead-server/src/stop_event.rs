//! Windows has no SIGTERM, so the local server waits on a named event instead and
//! `trailhead server stop` sets it. The `Local\` namespace keeps it to the user's session.

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, GetLastError, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};

fn name(pid: u32) -> Vec<u16> {
    format!("Local\\trailhead-stop-{pid}").encode_utf16().chain([0]).collect()
}

/// Resolves when another process calls [`request`] with this process's pid.
pub(crate) async fn wait() {
    let name = name(std::process::id());
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call.
    let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) };
    if handle.is_null() {
        tracing::warn!(error = %std::io::Error::last_os_error(), "couldn't create the stop event");
        return std::future::pending().await;
    }
    // Raw handles aren't Send; the event lives for the rest of the process, so it's never closed.
    let handle = handle as usize;
    let signaled = tokio::task::spawn_blocking(move || {
        // SAFETY: the handle came from CreateEventW above and is never closed.
        unsafe { WaitForSingleObject(handle as _, INFINITE) == WAIT_OBJECT_0 }
    })
    .await;
    if !matches!(signaled, Ok(true)) {
        std::future::pending::<()>().await;
    }
}

/// Asks the server with this pid to shut down gracefully. `Ok(false)` means it has no
/// stop event, e.g. it isn't a Trailhead server or predates this mechanism.
pub fn request(pid: u32) -> std::io::Result<bool> {
    let name = name(pid);
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call.
    let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
    if handle.is_null() {
        // SAFETY: no other Win32 call has run on this thread since OpenEventW.
        return match unsafe { GetLastError() } {
            ERROR_FILE_NOT_FOUND => Ok(false),
            code => Err(std::io::Error::from_raw_os_error(code as i32)),
        };
    }
    // SAFETY: `handle` is a valid event handle opened above with EVENT_MODIFY_STATE.
    let ok = unsafe { SetEvent(handle) } != 0;
    let err = std::io::Error::last_os_error();
    // SAFETY: `handle` is valid and closed exactly once.
    unsafe { CloseHandle(handle) };
    if ok { Ok(true) } else { Err(err) }
}
