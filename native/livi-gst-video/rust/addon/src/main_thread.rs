//! Cocoa wants its views touched on the main thread, which is Electron's.

use core::ffi::c_void;

unsafe extern "C" {
    fn livi_run_on_main(work: extern "C" fn(*mut c_void), context: *mut c_void);
}

struct Job<F, R> {
    f: Option<F>,
    out: Option<R>,
}

extern "C" fn run<F: FnOnce() -> R, R>(context: *mut c_void) {
    // SAFETY: the context is the job in on_main, alive until the call there returns.
    let job = unsafe { &mut *context.cast::<Job<F, R>>() };
    if let Some(f) = job.f.take() {
        job.out = Some(f());
    }
}

/// Safe to call from the main thread itself.
pub fn on_main<F: FnOnce() -> R + Send, R: Send>(f: F) -> R {
    let mut job = Job { f: Some(f), out: None };
    // SAFETY: livi_run_on_main returns after `run` did, so the job outlives it.
    unsafe { livi_run_on_main(run::<F, R>, (&raw mut job).cast()) };
    job.out.expect("the main thread ran the job")
}
