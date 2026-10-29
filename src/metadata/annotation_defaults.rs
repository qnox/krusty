//! Format support for annotation constructor default values in `ValueParameter` field 8.

/// FIR's serializer gates the payload on the physical metadata version, independently of the
/// source-language feature that enables declaration annotation records.
pub(crate) fn supported([major, minor, _]: [i32; 3]) -> bool {
    major > 2 || (major == 2 && minor >= 2)
}

#[cfg(test)]
mod tests {
    #[test]
    fn annotation_defaults_follow_physical_metadata_versions() {
        for (version, expected) in [
            ([1, 4, 1], false),
            ([2, 1, 99], false),
            ([2, 2, 0], true),
            ([2, 4, 0], true),
            ([3, 0, 0], true),
        ] {
            assert_eq!(super::supported(version), expected, "{version:?}");
        }
    }
}
