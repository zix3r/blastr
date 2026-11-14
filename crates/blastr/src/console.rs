//! Console ownership handling for the GUI.

/// Detaches from the console when this process owns it alone.
///
/// On Windows a console program started from Explorer or a shortcut gets its
/// own console window. When the GUI is about to open, that window is not
/// wanted. A console shared with a shell (the program was started from a
/// terminal) is left alone.
pub fn detach_if_owned() {
    #[cfg(windows)]
    detach_owned_console();
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn detach_owned_console() {
    use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};

    let mut processes = [0u32; 2];
    // SAFETY: `processes` points to two writable `u32` values and the buffer
    // size passed alongside it is exactly that length.
    let count = unsafe { GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32) };
    if count == 1 {
        // SAFETY: the process has a console and owns it alone, so detaching it
        // needs no further preconditions and its result can be ignored.
        let _ = unsafe { FreeConsole() };
    }
}
