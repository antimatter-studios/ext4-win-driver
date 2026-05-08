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
        CreateWindowExW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE, DBT_DEVTYP_DEVICEINTERFACE,
        DEVICE_NOTIFY_WINDOW_HANDLE, DEV_BROADCAST_DEVICEINTERFACE_W, DefWindowProcW,
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

        // Now we're ready -- accept Stop + Shutdown.
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
        let drained: Vec<((String, usize), char)> = st.mounts.drain().collect();
        drop(st);
        let launcher = LauncherClient::locate().ok();
        for ((dev, n), letter) in drained {
            if let Some(l) = &launcher {
                if let Err(e) = l.stop(letter) {
                    eprintln!("[service] launchctl stop {letter}: {e:#} ({dev}#part{n})");
                }
            }
            println!(
                "[service] {dev}#part{n} -> launchctl stop ext4-mount {letter} (shutdown)"
            );
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

            // Disk-class device-interface filter. We listen at disk
            // level (not volume level) because Windows refuses to
            // assign drive letters to MBR partitions whose type code
            // it doesn't recognise -- typical Linux ext4 SD cards
            // never surface as a drive letter, so a volume-level
            // subscription would never fire for them. The disk
            // arrival fires regardless; the wndproc opens the disk
            // path from the lparam payload, walks the partition
            // table, and probes each partition at its on-disk offset.
            let mut filter: DEV_BROADCAST_DEVICEINTERFACE_W = std::mem::zeroed();
            filter.dbcc_size = std::mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32;
            filter.dbcc_devicetype = DBT_DEVTYP_DEVICEINTERFACE;
            filter.dbcc_classguid = probe::GUID_DEVINTERFACE_DISK;
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
        /// `(disk_device_path, 1-indexed-partition)` -> mount drive
        /// letter we asked WinFsp.Launcher to host. `disk_device_path`
        /// is whatever the WM_DEVICECHANGE lparam reported -- typically
        /// `\\?\STORAGE#Disk#{guid}#...` -- so removal lookups match
        /// arrivals byte-for-byte.
        mounts: HashMap<(String, usize), char>,
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
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Mutex<State>;
                let bdi = lparam as *const DEV_BROADCAST_DEVICEINTERFACE_W;
                if !state_ptr.is_null() {
                    if let Some(disk_path) = probe::device_interface_name(bdi) {
                        let state: &Mutex<State> = &*state_ptr;
                        if event == DBT_DEVICEARRIVAL {
                            handle_arrival(state, &disk_path);
                        } else {
                            handle_removal(state, &disk_path);
                        }
                    }
                }
            }
            return 1;
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// Disk arrived. Open the disk for raw read, walk MBR/GPT, probe
    /// each partition for ext4, ask WinFsp.Launcher to spawn the mount
    /// for each hit. If the disk has no partition table, treat it as
    /// a single raw ext4 fs.
    fn handle_arrival(state: &Mutex<State>, disk_path: &str) {
        if state.lock().map(|s| s.shutting_down).unwrap_or(true) {
            return;
        }
        // No active console session = nothing to mount into. Skip --
        // a re-plug after login will retry.
        if active_console_session().is_none() {
            eprintln!(
                "[service] {disk_path}: no active console session, deferring mount"
            );
            return;
        }

        let parts = match crate::partition::list(Path::new(disk_path)) {
            Ok(v) => v,
            Err(e) => {
                if format!("{e:#}").contains("no MBR signature") {
                    spawn_partition_mount(state, disk_path, 0);
                } else {
                    eprintln!("[service] partition::list({disk_path}) failed: {e:#}");
                }
                return;
            }
        };

        for (idx, part) in parts.iter().enumerate() {
            let n = idx + 1;
            let part_offset = part.start_lba * 512;
            match probe_at_offset(disk_path, part_offset) {
                Ok(true) => spawn_partition_mount(state, disk_path, n),
                Ok(false) => {}
                Err(e) => eprintln!("[service] probe {disk_path} part {n}: {e:#}"),
            }
        }
    }

    /// Disk removed. Stop every WinFsp.Launcher service we started
    /// for partitions of this disk. Lookups by exact `disk_path`
    /// match because arrivals and removals report the same string.
    fn handle_removal(state: &Mutex<State>, disk_path: &str) {
        let stops: Vec<((String, usize), char)> = {
            let mut st = match state.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let keys: Vec<(String, usize)> = st
                .mounts
                .keys()
                .filter(|(d, _)| d == disk_path)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|k| st.mounts.remove(&k).map(|v| (k, v)))
                .collect()
        };
        if stops.is_empty() {
            return;
        }
        let launcher = match LauncherClient::locate() {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[service] cannot locate launchctl: {e:#}");
                return;
            }
        };
        for ((_, n), letter) in stops {
            match launcher.stop(letter) {
                Ok(()) => println!(
                    "[service] {disk_path}#part{n} removed -> launchctl stop {LAUNCHER_SERVICE_CLASS} {letter}"
                ),
                Err(e) => eprintln!("[service] launchctl stop {letter}: {e:#}"),
            }
        }
    }

    /// Read a small sector-aligned window at `offset` from
    /// `disk_path` and check for the ext4 superblock magic. 4 KiB
    /// covers both 512-byte and 4Kn devices and is large enough to
    /// reach `s_magic` at offset 0x438.
    fn probe_at_offset(disk_path: &str, offset: u64) -> Result<bool> {
        use crate::device::BlockSource;
        let src = crate::device::FileSource::open(Path::new(disk_path))
            .with_context(|| format!("opening {disk_path} for probe"))?;
        let mut buf = vec![0u8; 4096];
        if src.read_at(offset, &mut buf).is_err() {
            return Ok(false);
        }
        Ok(probe::is_ext4(&buf))
    }

    /// Pick a free drive letter and ask WinFsp.Launcher to spawn the
    /// mount in the active console session. Tracks the mount in
    /// `State.mounts` keyed by `(disk_path, n)` so removal can stop it.
    fn spawn_partition_mount(state: &Mutex<State>, disk_path: &str, n: usize) {
        let mount_letter = match probe::pick_drive_letter() {
            Some(c) => c,
            None => {
                eprintln!(
                    "[service] {disk_path}#part{n}: ext4 detected but no free drive letter"
                );
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
            "[service] ext4 detected on {disk_path}#part{n} -> launchctl start {LAUNCHER_SERVICE_CLASS} {mount_letter} {disk_path} {n}"
        );

        match launcher.start(mount_letter, disk_path, n) {
            Ok(()) => {
                if let Ok(mut st) = state.lock() {
                    st.mounts.insert((disk_path.to_string(), n), mount_letter);
                }
            }
            Err(e) => eprintln!("[service] launchctl start failed: {e:#}"),
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

        fn start(&self, letter: char, disk_path: &str, partition: usize) -> Result<()> {
            let letter_s = format!("{letter}");
            // `launchctl-<arch> start <ClassName> <InstanceName>
            // [TemplateArgs...]`. WinFsp.Launcher substitutes
            // template args into the registered CommandLine starting
            // at `%1` -- the InstanceName itself is not in the
            // substitution table. So we pass the drive letter twice:
            // once as InstanceName (so `sc query` shows distinct
            // services for concurrent mounts), once as the first
            // template arg.
            //   %1 = drive letter
            //   %2 = disk device path
            //   %3 = 1-indexed partition number, or "0" to mean
            //        "no partition table; treat the whole device
            //        as the ext4 fs"
            // The registered CommandLine in Product.wxs is
            //   `mount %2 --drive %1 --part %3`.
            let part_s = format!("{partition}");
            let drive_arg = format!("{letter}:"); // ext4 mount --drive expects "X:"
            let status = Command::new(&self.exe)
                .arg("start")
                .arg(LAUNCHER_SERVICE_CLASS)
                .arg(&letter_s) // InstanceName
                .arg(&drive_arg) // %1 -- substituted into `--drive %1`
                .arg(disk_path) // %2
                .arg(&part_s)   // %3
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
