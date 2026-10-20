//! The text of each diagnostic that a language feature migrates from a deprecation warning to an
//! error. The feature application that turns it into the warning's text (`parser::file_features`)
//! and the checker that reports it both read it here, so neither depends on the other.

/// `ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier`: a package qualifier wrapped
/// in parentheses.
pub(crate) const PARENTHESIZED_PACKAGE_QUALIFIER: &str =
    "wrapping package qualifiers in parentheses is not allowed.";

/// `ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier`: type arguments on a package
/// segment.
pub(crate) const PACKAGE_QUALIFIER_TYPE_ARGUMENTS: &str =
    "type arguments are not allowed for packages.";

/// `ErrorAboutDataClassCopyVisibilityChange`: a data class whose non-public primary constructor its
/// generated `copy()` exposes.
pub(crate) const DATA_CLASS_COPY_VISIBILITY: &str = "non-public primary constructor is exposed via the generated 'copy()' method of the 'data' class.\n\nThe generated 'copy()' will change its visibility in future releases.\n\nTo suppress the warning do one of the following:\n- Annotate the data class with the '@ConsistentCopyVisibility' annotation.\n- Use the '-Xconsistent-data-class-copy-visibility' compiler flag.\n- Annotate the data class with the '@ExposedCopyVisibility' annotation \n  (Discouraged, but can be used to keep binary compatibility).\n\nTo learn more, see the documentation of the '@ConsistentCopyVisibility' and '@ExposedCopyVisibility' annotations.\n\n";

/// `ErrorAboutDataClassCopyVisibilityChange`: a use of that `copy()`.
pub(crate) const DATA_CLASS_INVISIBLE_COPY_USAGE: &str = "this 'copy()' exposes the non-public primary constructor of a 'data class'. See the appropriate 'data class' documentation or contact the 'data class' author for migration guidance.";
