//! Per-compilation Wasm array bounds policy, including the reference release's default.

use crate::kotlin_version::{KotlinVersion, ReleaseRecords};

const RANGE_CHECKS: ReleaseRecords<bool> = ReleaseRecords::new(&[
    (KotlinVersion::V2_4_0, false),
    (KotlinVersion::V2_4_20, true),
]);

pub(super) fn default_range_checks() -> bool {
    *RANGE_CHECKS
        .get(crate::kotlin_version::target())
        .expect("every supported Kotlin release has a Wasm bounds default")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_defaults_cover_the_supported_release_manifest() {
        assert_eq!(RANGE_CHECKS.check(), Ok(()));
    }
}
