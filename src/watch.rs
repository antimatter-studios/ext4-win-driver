//! Auto-mount watcher for Windows volume arrival/removal.
//!
//! Runs as a foreground process. On Windows, listens for
//! `WM_DEVICECHANGE` (`DBT_DEVICEARRIVAL` / `DBT_DEVICEREMOVECOMPLETE`)
//! filtered to volume-class device-interface notifications
//! (`GUID_DEVINTERFACE_VOLUME`), diffs `GetLogicalDrives` against the
//! last seen mask to find which letter actually changed, probes each
//! newly-arrived volume for an ext4 superblock, and on a hit spawns
//! `ext4 mount <device> --drive <letter>` as a child process. On
//! removal, kills the child so its WinFsp `Drop` tears the mount down
//! cleanly.
//!
//! Self-contained: never links WinFsp directly. The current binary is
//! re-exec'd with the `mount` subcommand — that subcommand is the one
//! gated on the `mount` feature + Windows target.
//!
//! On non-Windows the [`run`] entrypoint just prints a hint and returns
//! `Ok(())` so the CLI dispatcher remains uniform.
//!
//! Event source: `RegisterDeviceNotification` + a hidden message-only
//! window. Picked over WMI because `windows-sys` already covers it; no
//! new dep cost. The downside (a message pump on the calling thread) is
//! tiny — the watcher's whole job is to sit in that pump.

use anyhow::Result;

#[cfg(target_os = "windows")]
pub fn run() -> Result<()> {
    imp::run()
}

#[cfg(not(target_os = "windows"))]
pub fn run() -> Result<()> {
    eprintln!("[watch] is Windows-only — this build is non-Windows, exiting.");
    Ok(())
}

#[cfg(target_os = "windows")]
mod imp {
    //! Windows implementation. Layout:
    //!
    //!   1. `State` — children map + Win32 handles, behind `Mutex`.
    //!   2. `run` — install Ctrl-C handler, create message-only window,
    //!      register volume notifications, pump until WM_QUIT.
    //!   3. `wnd_proc` — handle WM_DEVICECHANGE, dispatch into State.
    //!
    //! Probe + drive-letter helpers live in [`crate::probe`] so the
    //! service variant ([`crate::service`]) shares them.

    use anyhow::{Context, Result, anyhow};
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::process::{Child, Command};
    use std::ptr;
    use std::sync::{Mutex, OnceLock};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE, DBT_DEVTYP_DEVICEINTERFACE,
        DEVICE_NOTIFY_WINDOW_HANDLE, DEV_BROADCAST_DEVICEINTERFACE_W, DefWindowProcW,
        DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW,
        HWND_MESSAGE, MSG, PostThreadMessageW, RegisterClassW, RegisterDeviceNotificationW,
        SetWindowLongPtrW, TranslateMessage, UnregisterClassW, UnregisterDeviceNotification,
        WM_DEVICECHANGE, WM_QUIT, WNDCLASSW,
    };

    use crate::probe;

    /// Window class name. Whatever — just needs to be unique to this
    /// process. Wide-encoded inline to avoid bringing in widestring.
    const CLASS_NAME: &[u16] = &[
        b'e' as u16, b'x' as u16, b't' as u16, b'4' as u16, b'_' as u16, b'w' as u16,
        b'a' as u16, b't' as u16, b'c' as u16, b'h' as u16, 0,
    ];

    /// Shared between the wndproc and the Ctrl-C handler. Single instance
    /// per process (a second `watch` invocation in the same process would
    /// be a programming error). `OnceLock` so we initialise lazily and
    /// give the wndproc a stable pointer for `GWLP_USERDATA`.
    fn state() -> &'static Mutex<State> {
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        STATE.get_or_init(|| Mutex::new(State::default()))
    }

    #[derive(Default)]
    struct State {
        /// Drive-letter device path (`\\.\E:`) -> spawned `ext4 mount` child.
        children: HashMap<String, Child>,
        /// Set on Ctrl-C / WM_QUIT path; the wndproc consults this so it
        /// stops spawning new mounts during shutdown.
        shutting_down: bool,
        /// Last `GetLogicalDrives` snapshot. The wndproc diffs the
        /// current mask against this on every WM_DEVICECHANGE arrival /
        /// removal to find which letter(s) actually changed -- the
        /// message itself is treated as a wake-up only.
        last_drive_mask: u32,
    }

    pub fn run() -> Result<()> {
        // Take a stable static pointer to State — passed to the wndproc
        // via GWLP_USERDATA so it can find us during message dispatch.
        let state_ptr = state() as *const Mutex<State> as isize;

        // Install Ctrl-C handler. Posts WM_QUIT to wake the message
        // pump, which then unwinds via the cleanup at end of `run`.
        let main_thread = unsafe { GetCurrentThreadId() };
        ctrlc::set_handler(move || {
            // Mark shutdown so any in-flight WM_DEVICECHANGE doesn't
            // race a new spawn against our cleanup.
            if let Ok(mut s) = state().lock() {
                s.shutting_down = true;
            }
            unsafe {
                PostThreadMessageW(main_thread, WM_QUIT, 0, 0);
            }
        })
        .context("installing Ctrl-C handler")?;

        unsafe { run_pump(state_ptr) }
    }

    /// Build the message-only window, register for volume events, drain
    /// the message pump until WM_QUIT, then tear everything down.
    unsafe fn run_pump(state_ptr: isize) -> Result<()> {
        let hinstance = GetModuleHandleW(ptr::null());

        // Register window class (idempotent failure is fine — a previous
        // `watch` run in the same process may have registered already).
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wnd_proc);
        wc.hInstance = hinstance;
        wc.lpszClassName = CLASS_NAME.as_ptr();
        let atom = RegisterClassW(&wc);
        if atom == 0 {
            // Re-using an existing class is fine; otherwise propagate the
            // last OS error so the user sees it.
            let err = std::io::Error::last_os_error();
            // ERROR_CLASS_ALREADY_EXISTS = 1410
            if err.raw_os_error() != Some(1410) {
                return Err(anyhow!("RegisterClassW failed: {err}"));
            }
        }

        let hwnd: HWND = CreateWindowExW(
            0,
            CLASS_NAME.as_ptr(),
            ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            ptr::null_mut(),
            hinstance,
            ptr::null(),
        );
        if hwnd.is_null() {
            return Err(anyhow!(
                "CreateWindowExW(HWND_MESSAGE) failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        // Stash state pointer so wnd_proc can find it. SetWindowLongPtrW
        // returns the previous value (0 on first set), and to detect a
        // genuine error we'd need to clear+check GetLastError. Skipped —
        // we own this freshly-created window.
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr);

        // Subscribe to volume-class device-interface notifications.
        // `RegisterDeviceNotificationW` only accepts
        // DEV_BROADCAST_DEVICEINTERFACE_W (with a class GUID) or
        // DEV_BROADCAST_HANDLE; passing DEV_BROADCAST_VOLUME -- which
        // is what the WM_DEVICECHANGE *payload* uses for volume events
        // -- is rejected with ERROR_INVALID_DATA. We ignore the
        // notification's lparam and rely on `probe::diff_drives` from
        // the wndproc as the source of truth for what changed.
        let mut filter: DEV_BROADCAST_DEVICEINTERFACE_W = std::mem::zeroed();
        filter.dbcc_size = std::mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32;
        filter.dbcc_devicetype = DBT_DEVTYP_DEVICEINTERFACE;
        filter.dbcc_classguid = probe::GUID_DEVINTERFACE_VOLUME;
        let dev_handle = RegisterDeviceNotificationW(
            hwnd,
            &filter as *const _ as *const _,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
        if dev_handle.is_null() {
            DestroyWindow(hwnd);
            return Err(anyhow!(
                "RegisterDeviceNotificationW failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        // Seed the drive-mask baseline before we start pumping so the
        // first arrival sees a delta against the boot state, not 0.
        if let Ok(mut s) = state().lock() {
            s.last_drive_mask = probe::current_drive_mask();
        }

        println!("[watch] listening for ext4 volume arrivals. Ctrl-C to stop.");

        // Message pump. GetMessageW returns 0 on WM_QUIT (the Ctrl-C
        // path posts that), -1 on error, anything else means dispatch.
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let r = GetMessageW(&mut msg, ptr::null_mut(), 0, 0);
            if r == 0 {
                break;
            }
            if r == -1 {
                eprintln!(
                    "[watch] GetMessageW error: {}",
                    std::io::Error::last_os_error()
                );
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // Cleanup: unregister, kill children, destroy window.
        UnregisterDeviceNotification(dev_handle);
        DestroyWindow(hwnd);
        UnregisterClassW(CLASS_NAME.as_ptr(), hinstance);

        let mut st = state().lock().unwrap();
        st.shutting_down = true;
        let drained: Vec<(String, Child)> = st.children.drain().collect();
        drop(st);
        for (dev, mut child) in drained {
            // Best-effort kill + reap; child's WinFsp Drop unmounts.
            let _ = child.kill();
            let _ = child.wait();
            println!("[watch] {dev} → child unmounted (shutdown)");
        }
        Ok(())
    }

    /// Window procedure. Dispatches WM_DEVICECHANGE; everything else
    /// falls through to DefWindowProcW.
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_DEVICECHANGE {
            let event = wparam as u32;
            // Treat the message as a pure wake-up: diff the current
            // drive-letter mask against the last seen one to discover
            // what actually changed. The lparam payload would be a
            // DEV_BROADCAST_DEVICEINTERFACE_W (since we registered with
            // GUID_DEVINTERFACE_VOLUME), but converting its device path
            // to a drive letter is fiddly; GetLogicalDrives is the
            // shorter route.
            if event == DBT_DEVICEARRIVAL || event == DBT_DEVICEREMOVECOMPLETE {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Mutex<State>;
                if !state_ptr.is_null() {
                    let state: &Mutex<State> = &*state_ptr;
                    let prev = state
                        .lock()
                        .map(|s| s.last_drive_mask)
                        .unwrap_or_else(|_| probe::current_drive_mask());
                    let (cur, added, removed) = probe::diff_drives(prev);
                    if let Ok(mut s) = state.lock() {
                        s.last_drive_mask = cur;
                    }
                    for letter in added {
                        handle_arrival(state, letter);
                    }
                    for letter in removed {
                        handle_removal(state, letter);
                    }
                }
            }
            // Per docs, return TRUE to grant any pending request; for
            // arrivals/removals the return value is ignored, so 1 is fine.
            return 1;
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// `DBT_DEVICEARRIVAL` for a volume: probe the new drive letter,
    /// and if it carries an ext4 superblock, spawn `ext4 mount …`.
    fn handle_arrival(state: &Mutex<State>, letter: char) {
        // Don't spawn during shutdown.
        if state.lock().map(|s| s.shutting_down).unwrap_or(true) {
            return;
        }

        let dev = format!("\\\\.\\{letter}:");
        let dev_path = Path::new(&dev);

        match probe::probe_path(dev_path) {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                eprintln!("[watch] probe {dev} failed: {e:#}");
                return;
            }
        }

        let mount_letter = match probe::pick_drive_letter() {
            Some(c) => c,
            None => {
                eprintln!("[watch] {dev} ext4 detected but no free drive letter");
                return;
            }
        };

        println!("[watch] ext4 detected on {dev} → mounting on {mount_letter}:");

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[watch] current_exe() failed: {e}");
                return;
            }
        };
        let drive_arg = format!("{mount_letter}:");
        match Command::new(&exe)
            .arg("mount")
            .arg(&dev)
            .arg("--drive")
            .arg(&drive_arg)
            .spawn()
        {
            Ok(child) => {
                let mut st = match state.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                // Track by the *source* device path so removal lookups
                // match what Windows reports.
                st.children.insert(dev.clone(), child);
                // Stash the mount letter in a tiny side-channel so the
                // removal log can show "unmounted from F:". We piggyback
                // on a separate map to keep the Child handling unchanged.
                let _ = mount_letters().lock().map(|mut m| {
                    m.insert(dev, mount_letter);
                });
            }
            Err(e) => {
                eprintln!("[watch] spawn `ext4 mount {dev} --drive {drive_arg}` failed: {e}");
            }
        }
    }

    /// `DBT_DEVICEREMOVECOMPLETE` for a volume: kill the matching child.
    fn handle_removal(state: &Mutex<State>, letter: char) {
        let dev = format!("\\\\.\\{letter}:");
        let mut st = match state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some(mut child) = st.children.remove(&dev) {
            let mount_letter_str = mount_letters()
                .lock()
                .ok()
                .and_then(|mut m| m.remove(&dev))
                .map(|c| format!("{c}:"))
                .unwrap_or_else(|| "?".into());
            // Best-effort: WinFsp's Drop in the child should still
            // tear the mount down once the process exits.
            let _ = child.kill();
            let _ = child.wait();
            println!("[watch] {dev} removed → unmounted from {mount_letter_str}");
        }
    }

    /// Tiny side-table keyed by source device → mount letter, just for
    /// logging on removal. Not needed for correctness; a clean impl
    /// would fold this into `State.children` as `(Child, char)` instead.
    fn mount_letters() -> &'static Mutex<HashMap<String, char>> {
        static M: OnceLock<Mutex<HashMap<String, char>>> = OnceLock::new();
        M.get_or_init(|| Mutex::new(HashMap::new()))
    }

    // OsStr → wide silencer kept for potential future use (path
    // conversion when probing whole-disk arrivals via PhysicalDriveN).
    // TODO(whole-disk): when DBT_DEVTYP_HANDLE / PhysicalDriveN events
    // arrive, walk partition::list_from_source and probe each slice.
    #[allow(dead_code)]
    fn os_to_wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }
}
