//! The JDK a codegen/box test compiles against.
//!
//! JetBrains' test infrastructure (`JvmEnvironmentConfigurator.extractJdkKind`,
//! `getJdkClasspathRoot`, and `configureCompilerConfiguration`) compiles every box test against the
//! Java 7 mock JDK (`third-party/mockJDKs/mockJDK/jre/lib/rt.jar`, with `NO_JDK`) unless the test
//! declares `// FULL_JDK`. `JDK_KIND` is rejected in `codegen/box` by `JdkKindBoxTestChecker`, so
//! those two kinds are the whole selection. Only the compile-time JDK changes; tests still run on
//! the real JVM.
//!
//! The gate, the survey, and the reference-compiler oracle all select through this module, so a test
//! never resolves against a different JDK surface in one of them.

use std::path::{Path, PathBuf};

use super::directive;

/// The JDK kind a box test declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxJdkKind {
    /// The pinned mock JDK, with the ambient JDK switched off.
    Mock,
    /// The real JDK the harness runs on (`// FULL_JDK`).
    Full,
}

pub fn box_jdk_kind(src: &str) -> BoxJdkKind {
    if directive(src, "FULL_JDK") {
        BoxJdkKind::Full
    } else {
        BoxJdkKind::Mock
    }
}

/// The two bootclasspath roots a box run can select from.
#[derive(Clone, Debug)]
pub struct BoxJdkRoots {
    mock_rt_jar: PathBuf,
    full_jdk: Option<PathBuf>,
}

/// One test's selected JDK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxJdk<'a> {
    /// Compile against exactly this mock `rt.jar`; the compiler must not add its own JDK.
    Mock { rt_jar: &'a Path },
    /// Compile against the real JDK. `root` is its bootclasspath root (`lib/modules`) when the
    /// caller supplies the JDK explicitly; a reference compiler uses the JDK it runs on.
    Full { root: Option<&'a Path> },
}

impl BoxJdkRoots {
    /// `mock_rt_jar` comes from the JetBrains checkout that holds the corpus (see
    /// [`crate::toolchain::box_mock_jdk_rt_jar`]); `full_jdk` is the running JDK's bootclasspath root.
    pub fn new(mock_rt_jar: PathBuf, full_jdk: Option<PathBuf>) -> Self {
        Self {
            mock_rt_jar,
            full_jdk,
        }
    }

    pub fn select(&self, src: &str) -> BoxJdk<'_> {
        match box_jdk_kind(src) {
            BoxJdkKind::Mock => BoxJdk::Mock {
                rt_jar: &self.mock_rt_jar,
            },
            BoxJdkKind::Full => BoxJdk::Full {
                root: self.full_jdk.as_deref(),
            },
        }
    }
}

impl<'a> BoxJdk<'a> {
    /// The classpath root that stands in for the JDK in an explicit-classpath compilation.
    pub fn classpath_root(self) -> Option<&'a Path> {
        match self {
            BoxJdk::Mock { rt_jar } => Some(rt_jar),
            BoxJdk::Full { root } => root,
        }
    }

    /// Reference `kotlinc` arguments that select this JDK: `-no-jdk` plus the mock jar as an
    /// ordinary classpath root, exactly as JetBrains' configurator sets `NO_JDK` and adds the jar.
    /// The full JDK needs no argument: the reference compiler uses the JDK it runs on.
    pub fn kotlinc_args(self, classpath: &[PathBuf]) -> Result<Vec<String>, String> {
        let mut roots = classpath.to_vec();
        let mut args = Vec::new();
        if let BoxJdk::Mock { rt_jar } = self {
            args.push("-no-jdk".to_string());
            roots.push(rt_jar.to_path_buf());
        }
        if !roots.is_empty() {
            let joined = std::env::join_paths(&roots)
                .map_err(|error| format!("invalid reference JVM classpath: {error}"))?;
            args.push("-classpath".to_string());
            args.push(joined.to_string_lossy().into_owned());
        }
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> BoxJdkRoots {
        BoxJdkRoots::new(
            PathBuf::from("/corpus/third-party/mockJDKs/mockJDK/jre/lib/rt.jar"),
            Some(PathBuf::from("/jdk/lib/modules")),
        )
    }

    #[test]
    fn a_box_test_compiles_against_the_mock_jdk_by_default() {
        let roots = roots();
        let selected = roots.select("// WITH_STDLIB\nfun box() = \"OK\"\n");
        assert_eq!(
            selected,
            BoxJdk::Mock {
                rt_jar: Path::new("/corpus/third-party/mockJDKs/mockJDK/jre/lib/rt.jar")
            }
        );
        assert_eq!(
            selected.kotlinc_args(&[PathBuf::from("/lib/kotlin-stdlib.jar")]),
            Ok(vec![
                "-no-jdk".to_string(),
                "-classpath".to_string(),
                "/lib/kotlin-stdlib.jar:/corpus/third-party/mockJDKs/mockJDK/jre/lib/rt.jar"
                    .to_string(),
            ])
        );
    }

    #[test]
    fn full_jdk_selects_the_real_jdk() {
        let roots = roots();
        let selected = roots.select("// FULL_JDK\nfun box() = \"OK\"\n");
        assert_eq!(
            selected,
            BoxJdk::Full {
                root: Some(Path::new("/jdk/lib/modules"))
            }
        );
        assert_eq!(
            selected.classpath_root(),
            Some(Path::new("/jdk/lib/modules"))
        );
        assert_eq!(selected.kotlinc_args(&[]), Ok(Vec::new()));
    }
}
