//! Auto-mount watcher for Windows disk arrival/removal.
//!
//! Runs as a foreground process. On Windows, listens for
//! `WM_DEVICECHANGE` (`DBT_DEVICEARRIVAL` / `DBT_DEVICEREMOVECOMPLETE`)
//! filtered to disk-class device-interface notifications
//! (`GUID_DEVINTERFACE_DISK`), reads the lparam's
//! `DEV_BROADCAST_DEVICEINTERFACE_W::dbcc_name` to get the disk's
//! device path (e.g. `\\?\STORAGE#Disk#{guid}#...`), opens it for raw
//! read, walks the MBR/GPT partition table, and probes each partition
//! for an ext4 superblock at its on-disk offset. On a hit, spawns
//! `ext4 mount <disk_path> --drive <letter>: --part <N>` as a child
//! process. On removal, kills every child we spawned for that disk so
//! its WinFsp `Drop` tears the mount down cleanly.
//!
//! Why disk-class rather than volume-class: Windows refuses to assign
//! drive letters to MBR partitions whose type code it doesn't
//! recognise (e.g. `0x83` Linux native), so a volume-class
//! subscription -- whose only useful "what changed" signal is a
//! `GetLogicalDrives` diff -- never fires for typical Linux ext4 SD
//! cards. Disk-class arrivals fire regardless of partition type.
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

    /// Per-mount bookkeeping: the spawned `ext4 mount` child plus the
    /// drive letter it was assigned, keyed in [`State::mounts`] by
    /// `(disk_path, partition_index)`.
    struct MountedChild {
        child: Child,
        letter: char,
    }

    #[derive(Default)]
    struct State {
        /// `(disk_device_path, 1-indexed-partition)` -> mount info.
        /// `disk_device_path` is whatever the WM_DEVICECHANGE lparam
        /// reported -- typically `\\?\STORAGE#Disk#{guid}#...` -- so
        /// removal lookups match arrivals byte-for-byte.
        mounts: HashMap<(String, usize), MountedChild>,
        /// Set on Ctrl-C / WM_QUIT path; the wndproc consults this so it
        /// stops spawning new mounts during shutdown.
        shutting_down: bool,
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

        // Subscribe to disk-class device-interface notifications. We
        // listen at the disk level (not volume level) because Windows
        // refuses to assign drive letters to partitions whose type
        // code it doesn't recognise -- typical Linux ext4 SD cards
        // (single 0x83 partition, or no partition table at all) never
        // surface as a drive letter, so a volume-level subscription
        // would never fire for them. The disk arrival fires regardless.
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

        println!("[watch] listening for ext4 disk arrivals. Ctrl-C to stop.");

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
        let drained: Vec<((String, usize), MountedChild)> = st.mounts.drain().collect();
        drop(st);
        for ((disk, part), mut mc) in drained {
            // Best-effort kill + reap; child's WinFsp Drop unmounts.
            let _ = mc.child.kill();
            let _ = mc.child.wait();
            println!(
                "[watch] {disk}#part{part} -> child unmounted from {}: (shutdown)",
                mc.letter
            );
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
            if event == DBT_DEVICEARRIVAL || event == DBT_DEVICEREMOVECOMPLETE {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Mutex<State>;
                let bdi = lparam
                    as *const windows_sys::Win32::UI::WindowsAndMessaging::DEV_BROADCAST_DEVICEINTERFACE_W;
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
            // Per docs, return TRUE to grant any pending request; for
            // arrivals/removals the return value is ignored, so 1 is fine.
            return 1;
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// `DBT_DEVICEARRIVAL` for a disk-class device interface: open the
    /// disk, walk its partition table, probe each partition for an
    /// ext4 superblock, and spawn `ext4 mount` for each hit.
    fn handle_arrival(state: &Mutex<State>, disk_path: &str) {
        if state.lock().map(|s| s.shutting_down).unwrap_or(true) {
            return;
        }

        let parts = match crate::partition::list(Path::new(disk_path)) {
            Ok(v) => v,
            Err(e) => {
                // No MBR signature? Treat the whole disk as a single
                // raw ext4 fs (common for `mkfs.ext4 /dev/mmcblk0`
                // without partitioning).
                if format!("{e:#}").contains("no MBR signature") {
                    spawn_partition_mount(state, disk_path, 0);
                } else {
                    eprintln!("[watch] partition::list({disk_path}) failed: {e:#}");
                }
                return;
            }
        };

        for (idx, part) in parts.iter().enumerate() {
            let n = idx + 1; // 1-indexed for `--part`
            let part_offset = part.start_lba * 512;
            match probe_at_offset(disk_path, part_offset) {
                Ok(true) => {
                    spawn_partition_mount(state, disk_path, n);
                }
                Ok(false) => {}
                Err(e) => {
                    eprintln!("[watch] probe {disk_path} part {n}: {e:#}");
                }
            }
        }
    }

    /// `DBT_DEVICEREMOVECOMPLETE` for a disk: kill every child we own
    /// for this disk (one per ext4 partition).
    fn handle_removal(state: &Mutex<State>, disk_path: &str) {
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
        for key in keys {
            if let Some(mut mc) = st.mounts.remove(&key) {
                let _ = mc.child.kill();
                let _ = mc.child.wait();
                println!(
                    "[watch] {disk_path}#part{} removed -> unmounted from {}:",
                    key.1, mc.letter
                );
            }
        }
    }

    /// Probe a single partition (or the whole disk when `offset == 0`)
    /// for an ext4 superblock. Reads a small sector-aligned window
    /// from `disk_path` at the given byte offset and checks the
    /// magic.
    ///
    /// Buffer size is rounded up to a multiple of 4096 because raw
    /// disk handles on Windows require sector-aligned counts (512 on
    /// classic drives, 4096 on 4Kn / advanced-format drives).
    /// `probe::is_ext4` only needs the first 1090 bytes to reach
    /// `s_magic`, so any 4 KiB read covers it.
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

    /// Pick a free drive letter and spawn `ext4 mount <disk_path>
    /// --drive <X:> [--part <N>]` (omitting `--part` when `n == 0` to
    /// signal whole-disk / no-partition-table). Tracks the spawned
    /// child in `State.mounts` keyed by `(disk_path, n)`.
    fn spawn_partition_mount(state: &Mutex<State>, disk_path: &str, n: usize) {
        let mount_letter = match probe::pick_drive_letter() {
            Some(c) => c,
            None => {
                eprintln!(
                    "[watch] {disk_path}#part{n}: ext4 detected but no free drive letter"
                );
                return;
            }
        };

        println!(
            "[watch] ext4 detected on {disk_path}#part{n} -> mounting on {mount_letter}:"
        );

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[watch] current_exe() failed: {e}");
                return;
            }
        };
        let drive_arg = format!("{mount_letter}:");
        let mut cmd = Command::new(&exe);
        cmd.arg("mount").arg(disk_path).arg("--drive").arg(&drive_arg);
        if n > 0 {
            cmd.arg("--part").arg(n.to_string());
        }
        match cmd.spawn() {
            Ok(child) => {
                if let Ok(mut st) = state.lock() {
                    st.mounts.insert(
                        (disk_path.to_string(), n),
                        MountedChild {
                            child,
                            letter: mount_letter,
                        },
                    );
                }
            }
            Err(e) => {
                eprintln!(
                    "[watch] spawn `ext4 mount {disk_path} --drive {drive_arg}{}` failed: {e}",
                    if n > 0 { format!(" --part {n}") } else { String::new() }
                );
            }
        }
    }
}
