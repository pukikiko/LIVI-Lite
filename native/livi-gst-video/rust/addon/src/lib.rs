#[cfg(target_os = "macos")]
mod control;
#[cfg(target_os = "macos")]
mod main_thread;
#[cfg(target_os = "macos")]
mod screens;
#[cfg(target_os = "macos")]
mod server;

#[cfg(target_os = "macos")]
pub mod api {
    use napi::bindgen_prelude::Buffer;
    use napi_derive::napi;

    /// The pointer Electron's getNativeWindowHandle hands over.
    fn window_handle(buf: &Buffer) -> usize {
        let bytes: &[u8] = buf.as_ref();
        bytes
            .get(..size_of::<usize>())
            .and_then(|b| b.try_into().ok())
            .map_or(0, usize::from_ne_bytes)
    }

    #[napi]
    pub fn serve(control: String, planes: String) -> bool {
        match crate::server::serve(control.as_ref(), planes.as_ref()) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("[video] cannot listen at {control} and {planes}: {e}");
                false
            }
        }
    }

    /// None once the window closed.
    #[napi]
    pub fn set_window(screen: String, handle: Option<Buffer>) {
        crate::screens::set_window(&screen, handle.as_ref().map_or(0, window_handle));
    }
}
