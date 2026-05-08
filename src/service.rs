//! Windows Service variant of the auto-mount watcher.
//!
//! Same volume-arrival logic as [`crate::watch`], but:
//!   - registered with the SCM via `windows-service::service_dispatcher`,
//!     so the binary can be started by `sc start ExtFsWatcher` /
//!     auto-started at boot;
//!   - mounts are launched via WinFsp.Launcher's `launchctl-<arch>.exe`
//!     rather than `Command::spawn` so the WinFsp service spawns the
//!     mount in the active console session (where Explorer can see it),
//!     not session 0 (where the LocalSystem service runs).
//!
//! Service-class name registered with WinFsp.Launcher: `ext4-mount`
//! (Stage 3 of the installer plan owns the registry side). When a
//! volume arrives we run:
//!
//! ```text
//! launchctl-<arch>.exe start ext4-mount <letter> <devpath>
//! ```
//!
//! and on removal:
//!
//! ```text
//! launchctl-<arch>.exe stop ext4-mount <letter>
//! ```
//!
//! The "service-name when calling Launcher" arg (`<letter>`) is
//! arbitrary but must be unique per concurrent mount — using the drive
//! letter is convenient and lines up with what we'd want to see in
//! `sc query`.
//!
//! Skeleton-friendliness: type names here (`Watcher`, `LauncherClient`)
//! are FS-agnostic. The only ext4-coupled bit is the call to
//! [`crate::probe::is_ext4`] and the `ext4-mount` service-class name —
//! the future skeleton extraction parameterises both.

// `Cmd::Service` is cfg(windows) in the dispatcher, so on macOS the
// stub `run` below is dead. Allow rather than duplicating cfg gates.
#![allow(dead_code)]

use anyhow::Result;

#[cfg(all(target_os = "windows", feature = "service"))]
pub fn run() -> Result<()> {
    imp::run()
}

/// Stub used when the `service` feature is off (the `windows-service`
/// dep isn't pulled in) or the target isn't Windows. Keeps the
/// dispatcher in `main.rs` uniform — the `Cmd::Service` variant is
/// itself cfg(windows), so on macOS this is unreachable; the
/// no-`service`-feature path on Windows builds reach this and print a
/// useful hint instead of silently no-op'ing.
#[cfg(not(all(target_os = "windows", feature = "service")))]
pub fn run() -> Result<()> {
    eprintln!(
        "[service] this build was compiled without the `service` feature; \
         rebuild with `--features service` on Windows to use the SCM dispatcher."
    );
    Ok(())
}

/// WinFsp.Launcher service-class name we register mounts under. Kept
/// here (as opposed to inlined into `imp`) so the non-Windows stub can
/// reference it without cfg gates and so the future skeleton extraction
/// has a single point to parameterise.
#[allow(dead_code)]
const LAUNCHER_SERVICE_CLASS: &str = "ext4-mount";

/// SCM service name. Skeleton-friendly: future qcow2/ntfs projects
/// based on the same skeleton can override this constant.
#[allow(dead_code)]
const SERVICE_NAME: &str = "ExtFsWatcher";

#[cfg(all(target_os = "windows", feature = "service"))]
mod imp {
    //! Windows implementation. Layout mirrors `watch::imp`:
    //!
    //!   1. `run` — entry point, hands control to the SCM dispatcher.
    //!   2. `service_main` — invoked by SCM on a worker thread; sets up
    //!      the control handler, reports `Running`, runs the pump.
    //!   3. `Watcher` — owns the message-only window + tracked mounts.
    //!   4. `LauncherClient` — locates `launchctl-<arch>.exe` and shells
    //!      out to it for `start` / `stop`.
    //!   5. helpers — drive-letter / unitmask code lives in
    //!      [`crate::probe`].
    //!
    //! The control-handler-to-pump signal path is `PostThreadMessageW
    //! (main_thread, WM_QUIT, ...)` — same as the foreground watcher.
    //! The ServiceControlAccept bits (Stop + Shutdown) are reported up
    //! to SCM so the user-facing `sc stop` / system shutdown both work.

    use anyhow::{Context, Result, anyhow};
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::ptr;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{
        self, ServiceControlHandlerResult, ServiceStatusHandle,
    };
    use windows_service::service_dispatcher;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE, DBT_DEVTYP_VOLUME,
        DEVICE_NOTIFY_WINDOW_HANDLE, DEV_BROADCAST_HDR, DEV_BROADCAST_VOLUME, DefWindowProcW,
        DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW,
        HWND_MESSAGE, MSG, PostThreadMessageW, RegisterClassW, RegisterDeviceNotificationW,
        SetWindowLongPtrW, TranslateMessage, UnregisterClassW, UnregisterDeviceNotification,
        WM_DEVICECHANGE, WM_QUIT, WNDCLASSW,
    };

    use crate::probe;
    use crate::service::{LAUNCHER_SERVICE_CLASS, SERVICE_NAME};

    /// Window class name. Distinct from `watch::imp::CLASS_NAME` so the
    /// unlikely "watcher running in the same process as the service"
    /// case doesn't collide. Wide-encoded inline to avoid widestring.
    const CLASS_NAME: &[u16] = &[
        b'e' as u16, b'x' as u16, b't' as u16, b'f' as u16, b's' as u16, b'_' as u16,
        b's' as u16, b'v' as u16, b'c' as u16, 0,
    ];

    // ----- macros ---------------------------------------------------------

    // The windows-service crate's `define_windows_service!` macro
    // generates the `extern "system"` FFI shim that the SCM calls into.
    // It expands to a function with the given name that takes the raw
    // argv from SCM and forwards into our typed `service_main`.
    windows_service::define_windows_service!(ffi_service_main, service_main);

    pub fn run() -> Result<()> {
        // Hand control to the SCM dispatcher. This blocks for the
        // lifetime of the service (returns when `service_main` exits).
        // SCM invokes `ffi_service_main` on a worker thread.
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .context("service_dispatcher::start (run as a service, not directly)")
    }

    /// Invoked by SCM on a dedicated worker thread. Argv is whatever
    /// was passed to `StartService` — we ignore it.
    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = service_main_inner() {
            eprintln!("[service] fatal: {e:#}");
            // Best-effort: tell SCM we stopped with an error so the
            // event log records it.
            if let Some(handle) = status_handle().get().cloned() {
                let _ = handle.set_service_status(ServiceStatus {
                    service_type: ServiceType::OWN_PROCESS,
                    current_state: ServiceState::Stopped,
                    controls_accepted: ServiceControlAccept::empty(),
                    exit_code: ServiceExitCode::ServiceSpecific(1),
                    checkpoint: 0,
                    wait_hint: Duration::default(),
                    process_id: None,
                });
            }
        }
    }

    /// Stable storage for the SCM status handle so the control handler
    /// closure (which is `'static`) can reach it. Set once in
    /// `service_main_inner`.
    fn status_handle() -> &'static OnceLock<ServiceStatusHandle> {
        static H: OnceLock<ServiceStatusHandle> = OnceLock::new();
        &H
    }

    fn service_main_inner() -> Result<()> {
        // Capture this thread's id so the control handler can post
        // WM_QUIT to it and unwind the message pump cleanly.
        let main_thread = unsafe { GetCurrentThreadId() };

        let event_handler = move |control| -> ServiceControlHandlerResult {
            match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    // Mark shutdown so any in-flight WM_DEVICECHANGE
                    // doesn't race a new launchctl spawn against our
                    // teardown.
                    if let Ok(mut s) = state().lock() {
                        s.shutting_down = true;
                    }
                    unsafe {
                        PostThreadMessageW(main_thread, WM_QUIT, 0, 0);
                    }
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };

        let handle = service_control_handler::register(SERVICE_NAME, event_handler)
            .context("service_control_handler::register")?;
        let _ = status_handle().set(handle.clone());

        // Tell SCM we're starting. Setting `Running` later (after the
        // pump is up) is what the docs recommend; some clients will
        // wait on a `StartPending` for tens of seconds otherwise.
        handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::StartPending,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        })?;

        // Set up the message-only window + RegisterDeviceNotificationW
        // before reporting Running so we don't drop arrivals that fire
        // between SCM's "the service is up" and our pump going live.
        let state_ptr = state() as *const Mutex<State> as isize;
        let pump = unsafe { Pump::open(state_ptr)? };

        // Now we're ready — accept Stop + Shutdown.
        handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        // Block until WM_QUIT.
        unsafe { pump.run() };

        // Tear down: deregister notifications, destroy window,
        // launchctl-stop any mounts we own.
        drop(pump);

        let mut st = state().lock().unwrap();
        st.shutting_down = true;
        let drained: Vec<(String, char)> = st.mounts.drain().collect();
        drop(st);
        let launcher = LauncherClient::locate().ok();
        for (dev, letter) in drained {
            if let Some(l) = &launcher {
                if let Err(e) = l.stop(letter) {
                    eprintln!("[service] launchctl stop {letter}: {e:#} ({dev})");
                }
            }
            println!("[service] {dev} -> launchctl stop ext4-mount {letter} (shutdown)");
        }

        // Report Stopped.
        handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        Ok(())
    }

    // ---------------------------------------------------------------------
    // Pump: window + notification registration, RAII teardown.
    // ---------------------------------------------------------------------

    struct Pump {
        hwnd: HWND,
        dev_handle: *mut std::ffi::c_void,
        hinstance: windows_sys::Win32::Foundation::HINSTANCE,
    }

    impl Pump {
        unsafe fn open(state_ptr: isize) -> Result<Self> {
            let hinstance = GetModuleHandleW(ptr::null());

            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wnd_proc);
            wc.hInstance = hinstance;
            wc.lpszClassName = CLASS_NAME.as_ptr();
            let atom = RegisterClassW(&wc);
            if atom == 0 {
                let err = std::io::Error::last_os_error();
                // ERROR_CLASS_ALREADY_EXISTS = 1410. Idempotent re-reg
                // is fine if the service is restarted in-process.
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

            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr);

            let mut filter: DEV_BROADCAST_VOLUME = std::mem::zeroed();
            filter.dbcv_size = std::mem::size_of::<DEV_BROADCAST_VOLUME>() as u32;
            filter.dbcv_devicetype = DBT_DEVTYP_VOLUME;
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

            Ok(Pump {
                hwnd,
                dev_handle,
                hinstance,
            })
        }

        unsafe fn run(&self) {
            let mut msg: MSG = std::mem::zeroed();
            loop {
                let r = GetMessageW(&mut msg, ptr::null_mut(), 0, 0);
                if r == 0 {
                    break;
                }
                if r == -1 {
                    eprintln!(
                        "[service] GetMessageW error: {}",
                        std::io::Error::last_os_error()
                    );
                    break;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    impl Drop for Pump {
        fn drop(&mut self) {
            unsafe {
                UnregisterDeviceNotification(self.dev_handle);
                DestroyWindow(self.hwnd);
                UnregisterClassW(CLASS_NAME.as_ptr(), self.hinstance);
            }
        }
    }

    // ---------------------------------------------------------------------
    // State + wndproc + arrival/removal handlers
    // ---------------------------------------------------------------------

    fn state() -> &'static Mutex<State> {
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        STATE.get_or_init(|| Mutex::new(State::default()))
    }

    #[derive(Default)]
    struct State {
        /// Source device path (`\\.\E:`) -> mount drive letter we asked
        /// WinFsp.Launcher to host. Tracked so removal events know which
        /// `launchctl stop` to issue and to skip volumes we never
        /// mounted.
        mounts: HashMap<String, char>,
        shutting_down: bool,
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_DEVICECHANGE {
            let event = wparam as u32;
            if event == DBT_DEVICEARRIVAL || event == DBT_DEVICEREMOVECOMPLETE {
                let hdr = lparam as *const DEV_BROADCAST_HDR;
                if !hdr.is_null() && (*hdr).dbch_devicetype == DBT_DEVTYP_VOLUME {
                    let vol = lparam as *const DEV_BROADCAST_VOLUME;
                    let unitmask = (*vol).dbcv_unitmask;
                    let state_ptr =
                        GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Mutex<State>;
                    if !state_ptr.is_null() {
                        let state: &Mutex<State> = &*state_ptr;
                        for letter in probe::unitmask_to_letters(unitmask) {
                            if event == DBT_DEVICEARRIVAL {
                                handle_arrival(state, letter);
                            } else {
                                handle_removal(state, letter);
                            }
                        }
                    }
                }
            }
            return 1;
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// Volume arrived. Probe for ext4, pick a mount letter, ask
    /// WinFsp.Launcher to spawn the mount in the active console
    /// session.
    fn handle_arrival(state: &Mutex<State>, letter: char) {
        if state.lock().map(|s| s.shutting_down).unwrap_or(true) {
            return;
        }

        let dev = format!("\\\\.\\{letter}:");
        let dev_path = Path::new(&dev);

        match probe::probe_path(dev_path) {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                eprintln!("[service] probe {dev} failed: {e:#}");
                return;
            }
        }

        // Active-console-session check: if no interactive user is
        // logged in (returns 0xFFFFFFFF), there's nothing to mount
        // *into*. Skip — a re-plug or login will retry.
        if active_console_session().is_none() {
            eprintln!("[service] {dev}: no active console session, deferring mount");
            return;
        }

        let mount_letter = match probe::pick_drive_letter() {
            Some(c) => c,
            None => {
                eprintln!("[service] {dev} ext4 detected but no free drive letter");
                return;
            }
        };

        let launcher = match LauncherClient::locate() {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[service] cannot locate launchctl: {e:#}");
                return;
            }
        };

        println!(
            "[service] ext4 detected on {dev} -> launchctl start {LAUNCHER_SERVICE_CLASS} {mount_letter} {dev}"
        );

        match launcher.start(mount_letter, &dev) {
            Ok(()) => {
                if let Ok(mut st) = state.lock() {
                    st.mounts.insert(dev, mount_letter);
                }
            }
            Err(e) => {
                eprintln!("[service] launchctl start failed: {e:#}");
            }
        }
    }

    /// Volume removed. If we mounted it, ask WinFsp.Launcher to stop
    /// the mount; otherwise ignore (don't stop random services).
    fn handle_removal(state: &Mutex<State>, letter: char) {
        let dev = format!("\\\\.\\{letter}:");
        let mount_letter = match state.lock() {
            Ok(mut st) => st.mounts.remove(&dev),
            Err(_) => return,
        };
        let Some(mount_letter) = mount_letter else {
            return;
        };
        match LauncherClient::locate() {
            Ok(l) => match l.stop(mount_letter) {
                Ok(()) => println!(
                    "[service] {dev} removed -> launchctl stop {LAUNCHER_SERVICE_CLASS} {mount_letter}"
                ),
                Err(e) => eprintln!("[service] launchctl stop {mount_letter}: {e:#}"),
            },
            Err(e) => eprintln!("[service] cannot locate launchctl: {e:#}"),
        }
    }

    // ---------------------------------------------------------------------
    // Active-console-session helper
    // ---------------------------------------------------------------------

    /// Returns `Some(session_id)` if there's an interactive user
    /// session, `None` if no user is logged in (WTSGetActiveConsoleSessionId
    /// returned 0xFFFFFFFF).
    ///
    /// We don't need to *use* the session id for anything (WinFsp.Launcher
    /// handles the session-zero->user-session jump itself), but
    /// detecting "no user" lets us skip rather than queue a mount that
    /// would be invisible.
    fn active_console_session() -> Option<u32> {
        // WTSGetActiveConsoleSessionId lives in
        // Win32::System::RemoteDesktop in some windows-sys versions; in
        // 0.59 it lives under Win32::System::Kernel. Avoid the import
        // dance by linking it directly.
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn WTSGetActiveConsoleSessionId() -> u32;
        }
        let id = unsafe { WTSGetActiveConsoleSessionId() };
        if id == 0xFFFF_FFFF { None } else { Some(id) }
    }

    // ---------------------------------------------------------------------
    // LauncherClient — locate launchctl-<arch>.exe + invoke it.
    // ---------------------------------------------------------------------

    /// Wraps a path to `launchctl-<arch>.exe` and provides typed
    /// `start` / `stop` calls. Cheap to clone (the path is owned).
    struct LauncherClient {
        exe: PathBuf,
    }

    impl LauncherClient {
        /// Discover `launchctl-<arch>.exe` via the WinFsp install dir
        /// recorded in the registry. Errors if WinFsp isn't installed.
        fn locate() -> Result<Self> {
            let install_dir = winfsp_install_dir()
                .context("locating WinFsp install dir (HKLM\\SOFTWARE\\WOW6432Node\\WinFsp\\InstallDir)")?;
            let exe = install_dir.join("bin").join(launchctl_exe_name());
            if !exe.exists() {
                return Err(anyhow!("launchctl not found at {}", exe.display()));
            }
            Ok(LauncherClient { exe })
        }

        fn start(&self, letter: char, dev: &str) -> Result<()> {
            let letter_s = format!("{letter}");
            // `launchctl-<arch> start <ClassName> <InstanceName>
            // [TemplateArgs...]`. We use the drive letter as the
            // unique InstanceName (so concurrent mounts don't collide
            // in `sc query`) and pass `<dev>` as the single template
            // arg WinFsp.Launcher substitutes into the registered
            // CommandLine. Stage 3 owns that registry side; assume the
            // template is `... %1` where %1 is the source device.
            let status = Command::new(&self.exe)
                .arg("start")
                .arg(LAUNCHER_SERVICE_CLASS)
                .arg(&letter_s)
                .arg(dev)
                .status()
                .with_context(|| format!("running {}", self.exe.display()))?;
            if !status.success() {
                return Err(anyhow!(
                    "{} start exited with {status}",
                    self.exe.display()
                ));
            }
            Ok(())
        }

        fn stop(&self, letter: char) -> Result<()> {
            let letter_s = format!("{letter}");
            let status = Command::new(&self.exe)
                .arg("stop")
                .arg(LAUNCHER_SERVICE_CLASS)
                .arg(&letter_s)
                .status()
                .with_context(|| format!("running {}", self.exe.display()))?;
            if !status.success() {
                return Err(anyhow!(
                    "{} stop exited with {status}",
                    self.exe.display()
                ));
            }
            Ok(())
        }
    }

    /// Name of the launchctl executable for the current architecture,
    /// per WinFsp's bin layout. WinFsp ships `launchctl-x64.exe`,
    /// `launchctl-x86.exe`, `launchctl-a64.exe`.
    fn launchctl_exe_name() -> &'static str {
        if cfg!(target_arch = "x86_64") {
            "launchctl-x64.exe"
        } else if cfg!(target_arch = "aarch64") {
            "launchctl-a64.exe"
        } else if cfg!(target_arch = "x86") {
            "launchctl-x86.exe"
        } else {
            // Fall through to x64 — WinFsp doesn't ship anything else,
            // and the binary is unlikely to ever target a non-x86/arm
            // Windows host.
            "launchctl-x64.exe"
        }
    }

    /// Read `HKLM\SOFTWARE\WOW6432Node\WinFsp\InstallDir`.
    fn winfsp_install_dir() -> Result<PathBuf> {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{
            HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_WOW64_32KEY, REG_SZ, RegCloseKey,
            RegOpenKeyExW, RegQueryValueExW,
        };

        // Path is recorded under WOW6432Node so 64-bit + 32-bit clients
        // see the same install. We open with KEY_WOW64_32KEY rather
        // than hard-coding the WOW path — works on 32-bit hosts too,
        // and is the documented WinFsp pattern.
        let subkey = wide_z("SOFTWARE\\WinFsp");
        let value_name = wide_z("InstallDir");

        let mut hkey: HKEY = ptr::null_mut();
        let r = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                0,
                KEY_QUERY_VALUE | KEY_WOW64_32KEY,
                &mut hkey,
            )
        };
        if r != ERROR_SUCCESS {
            return Err(anyhow!(
                "RegOpenKeyExW(HKLM\\SOFTWARE\\WinFsp) failed: code {r}"
            ));
        }

        // First call: get required size.
        let mut ty: u32 = 0;
        let mut size: u32 = 0;
        let r = unsafe {
            RegQueryValueExW(
                hkey,
                value_name.as_ptr(),
                ptr::null_mut(),
                &mut ty,
                ptr::null_mut(),
                &mut size,
            )
        };
        if r != ERROR_SUCCESS {
            unsafe { RegCloseKey(hkey) };
            return Err(anyhow!(
                "RegQueryValueExW(InstallDir) size query failed: code {r}"
            ));
        }
        if ty != REG_SZ {
            unsafe { RegCloseKey(hkey) };
            return Err(anyhow!("InstallDir is type {ty}, expected REG_SZ"));
        }

        // size is in bytes. Round up to a u16 count, +1 for the
        // trailing NUL just in case the registry value isn't terminated.
        let mut buf: Vec<u16> = vec![0u16; (size as usize / 2) + 1];
        let mut size2 = size;
        let r = unsafe {
            RegQueryValueExW(
                hkey,
                value_name.as_ptr(),
                ptr::null_mut(),
                &mut ty,
                buf.as_mut_ptr() as *mut u8,
                &mut size2,
            )
        };
        unsafe { RegCloseKey(hkey) };
        if r != ERROR_SUCCESS {
            return Err(anyhow!("RegQueryValueExW(InstallDir) failed: code {r}"));
        }

        // Trim trailing NUL(s).
        let len = buf.iter().take_while(|&&c| c != 0).count();
        let s = String::from_utf16(&buf[..len]).context("InstallDir not valid UTF-16")?;
        Ok(PathBuf::from(s))
    }

    /// UTF-8 -> wide-NUL-terminated.
    fn wide_z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
}
