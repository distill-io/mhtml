#![no_main]

//! Fuzz target: arbitrary bytes fed
//! through the full parser surface must never panic. It exercises both the
//! lenient pull iterator (parse + per-part `body()`) and the strict
//! `parse_all()` path, plus the archive-level accessors.

use libfuzzer_sys::fuzz_target;
use mhtml::Archive;

fuzz_target!(|data: &[u8]| {
    if let Ok(archive) = Archive::parse(data) {
        let _ = archive.creation_date();
        let _ = archive.snapshot_content_location();
        for part in archive.parts() {
            if let Ok(p) = part {
                let _ = p.body();
            }
        }
        let _ = archive.parse_all();
    }
});
