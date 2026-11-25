//! The JDK a `codegen/boxModernJdk` case runs on.
//!
//! JetBrains' `configureModernJavaWhenNeeded` gives every case below `testsWithJava<N>/` the
//! defaults `JDK_KIND: FULL_JDK_<N>` and `JVM_TARGET: <N>`: the case compiles against the full JDK
//! `N`, emits class files for it, and `JvmBoxRunner` runs it on that JDK, with `--enable-preview`
//! when the case declares `// ENABLE_JVM_PREVIEW`. `testsWithValhalla/` needs a Valhalla JDK that
//! has no release yet, which the meta test configurator skips, so it names no JDK here.

use std::path::{Component, Path};

use super::directive;

/// The directory beside `codegen/box` that holds the cases.
pub const DIRECTORY: &str = "boxModernJdk";

const RELEASE_DIRECTORY_PREFIX: &str = "testsWithJava";

/// The JDK one case runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModernJdk {
    /// The JDK's feature release, which is also the case's default `-jvm-target`.
    pub release: u16,
}

/// Whether `relative` (a path below `compiler/testData/codegen`) is a `boxModernJdk` case.
pub fn is_modern_jdk_case(relative: &Path) -> bool {
    relative.components().next() == Some(Component::Normal(DIRECTORY.as_ref()))
}

/// The JDK the case at `relative` (a path below `compiler/testData/codegen`) runs on, or `None`
/// for one outside `boxModernJdk` or in a directory that names no available JDK.
pub fn modern_jdk(relative: &Path) -> Option<ModernJdk> {
    let mut parts = relative.components();
    if parts.next()? != Component::Normal(DIRECTORY.as_ref()) {
        return None;
    }
    let Component::Normal(directory) = parts.next()? else {
        return None;
    };
    let release = directory
        .to_str()?
        .strip_prefix(RELEASE_DIRECTORY_PREFIX)?
        .parse()
        .ok()?;
    Some(ModernJdk { release })
}

/// `src` under the directives `configureModernJavaTest` defaults: the JVM target of the JDK, unless
/// the case declares its own or a `// JDK_RELEASE:`. JetBrains' configuration holds a release and a
/// different target side by side, which no kotlinc command line can: it refuses
/// `-Xjdk-release` with another `-jvm-target`, and takes the target from the release.
pub fn with_default_directives(src: &str, jdk: ModernJdk) -> String {
    let declares = |name: &str| {
        src.lines()
            .any(|line| line.trim_start().strip_prefix(name).is_some())
    };
    if declares("// JVM_TARGET:") || declares("// JDK_RELEASE:") {
        return src.to_string();
    }
    format!("// JVM_TARGET: {}\n{src}", jdk.release)
}

/// Whether the case's class files carry the preview marking, so its JVM needs `--enable-preview`.
pub fn needs_preview(src: &str) -> bool {
    directive(src, "ENABLE_JVM_PREVIEW")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_case_runs_on_the_jdk_its_directory_names() {
        for (path, release) in [
            ("boxModernJdk/testsWithJava11/varHandle.kt", 11),
            ("boxModernJdk/testsWithJava17/records/dataJvmRecord.kt", 17),
            ("boxModernJdk/testsWithJava21/typeSwitch/a.kt", 21),
        ] {
            assert_eq!(modern_jdk(Path::new(path)), Some(ModernJdk { release }));
            assert!(is_modern_jdk_case(Path::new(path)));
        }
    }

    #[test]
    fn valhalla_and_other_roots_name_no_jdk() {
        assert_eq!(
            modern_jdk(Path::new("boxModernJdk/testsWithValhalla/a.kt")),
            None
        );
        assert_eq!(modern_jdk(Path::new("box/testsWithJava17/a.kt")), None);
        assert!(is_modern_jdk_case(Path::new(
            "boxModernJdk/testsWithValhalla/a.kt"
        )));
        assert!(!is_modern_jdk_case(Path::new("box/a.kt")));
    }

    #[test]
    fn the_jdks_target_is_a_default_a_case_can_override() {
        let jdk = ModernJdk { release: 17 };
        assert_eq!(
            with_default_directives("fun box() = \"OK\"\n", jdk),
            "// JVM_TARGET: 17\nfun box() = \"OK\"\n"
        );
        let own = "// JVM_TARGET: 11\nfun box() = \"OK\"\n";
        assert_eq!(with_default_directives(own, jdk), own);
        let release = "// JDK_RELEASE: 8\nfun box() = \"OK\"\n";
        assert_eq!(with_default_directives(release, jdk), release);
    }

    #[test]
    fn only_a_case_declaring_the_directive_needs_preview() {
        assert!(needs_preview("// ENABLE_JVM_PREVIEW\nfun box() = \"OK\"\n"));
        assert!(!needs_preview("fun box() = \"OK\"\n"));
    }
}
