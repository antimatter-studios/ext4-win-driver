// build.rs — wire WinFsp's delayload flags when the `mount` feature is
// enabled and the target is Windows. Otherwise no-op so this builds on
// any platform.

fn main() {
    #[cfg(windows)]
    {
        if std::env::var("CARGO_FEATURE_MOUNT").is_ok() {
            let target = std::env::var("TARGET").unwrap_or_default();
            if target.contains("windows") {
                winfsp::build::winfsp_link_delayload();
            }
        }
    }
}
