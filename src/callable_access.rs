//! File-level callable access shared by signature and body selection.

use crate::types::Visibility;

/// Whether a top-level function or extension is visible from one source file.
///
/// A present declaration file identifies a callable from the current module. Private access needs
/// an exact caller/file match; missing provenance is never treated as visible. Dependency-internal
/// access remains provider-owned through `external_internal_accessible`.
pub(crate) fn source_callable_accessible(
    visibility: Visibility,
    declaration_file: Option<u32>,
    access_file: Option<u32>,
    external_internal_accessible: impl FnOnce() -> bool,
) -> bool {
    match visibility {
        Visibility::Public => true,
        Visibility::Internal => declaration_file.is_some() || external_internal_accessible(),
        Visibility::Private => access_file
            .zip(declaration_file)
            .is_some_and(|(access, declaration)| access == declaration),
        Visibility::PackagePrivate | Visibility::Protected => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_access_uses_visibility_and_exact_file_provenance() {
        let visible = |visibility, declaration_file, access_file, internal| {
            source_callable_accessible(visibility, declaration_file, access_file, || internal)
        };

        assert_eq!(
            [
                visible(Visibility::Public, None, None, false),
                visible(Visibility::Internal, Some(1), Some(2), false),
                visible(Visibility::Internal, None, Some(2), true),
                visible(Visibility::Internal, None, Some(2), false),
                visible(Visibility::Private, Some(1), Some(1), false),
                visible(Visibility::Private, Some(1), Some(2), false),
                visible(Visibility::Private, None, Some(1), false),
                visible(Visibility::Private, Some(1), None, false),
            ],
            [true, true, true, false, true, false, false, false]
        );
    }
}
