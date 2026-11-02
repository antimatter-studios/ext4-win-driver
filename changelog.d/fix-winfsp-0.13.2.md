### Fixed

- **Explorer no longer lists a directory for ever.** winfsp-rs 0.13.2 stops
  `set_name` writing a NUL into directory entries, which kept WinFSP from
  finding where a listing had got to.
