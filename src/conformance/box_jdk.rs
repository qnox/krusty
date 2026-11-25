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
    /// The installed JDK homes by feature release, for `boxModernJdk` cases.
    homes: std::collections::BTreeMap<u16, (PathBuf, PathBuf)>,
}

/// One test's selected JDK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxJdk<'a> {
    /// Compile against exactly this mock `rt.jar`; the compiler must not add its own JDK.
    Mock { rt_jar: &'a Path },
    /// Compile against the real JDK. `root` is its bootclasspath root (`lib/modules`) when the
    /// caller supplies the JDK explicitly; a reference compiler uses the JDK it runs on.
    Full { root: Option<&'a Path> },
    /// Compile against one installed JDK and name it to the reference compiler (`-jdk-home`): a
    /// `boxModernJdk` case, which runs on that JDK (`FULL_JDK_<N>`).
    Home {
        home: &'a Path,
        /// Its bootclasspath root (`lib/modules`).
        modules: &'a Path,
    },
}

impl BoxJdkRoots {
    /// `mock_rt_jar` comes from the JetBrains checkout that holds the corpus (see
    /// [`crate::toolchain::box_mock_jdk_rt_jar`]); `full_jdk` is the running JDK's bootclasspath root.
    pub fn new(mock_rt_jar: PathBuf, full_jdk: Option<PathBuf>) -> Self {
        Self {
            mock_rt_jar,
            full_jdk,
            homes: std::collections::BTreeMap::new(),
        }
    }

    /// The installed JDK homes by feature release, which [`BoxJdkRoots::for_modern_jdk`] reads.
    pub fn with_jdk_homes(mut self, homes: impl IntoIterator<Item = (u16, PathBuf)>) -> Self {
        self.homes = homes
            .into_iter()
            .map(|(release, home)| {
                let modules = home.join("lib").join("modules");
                (release, (home, modules))
            })
            .collect();
        self
    }

    /// The JDK a `boxModernJdk` case compiles against and runs on. A release with no installed JDK
    /// is an error, never a reason to use another one: the JDK surface changes member resolution.
    pub fn for_modern_jdk(&self, jdk: super::ModernJdk) -> Result<BoxJdk<'_>, String> {
        let (home, modules) = self.homes.get(&jdk.release).ok_or_else(|| {
            format!(
                "needs JDK {0} (set KRUSTY_JDK_{0}_HOME or JAVA_HOME_{0}_X64)",
                jdk.release
            )
        })?;
        Ok(BoxJdk::Home { home, modules })
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
            BoxJdk::Home { modules, .. } => Some(modules),
        }
    }

    /// The JDK installation a case runs on, when the case names one.
    pub fn home(self) -> Option<&'a Path> {
        match self {
            BoxJdk::Home { home, .. } => Some(home),
            BoxJdk::Mock { .. } | BoxJdk::Full { .. } => None,
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
        if let BoxJdk::Home { home, .. } = self {
            args.push("-jdk-home".to_string());
            args.push(home.to_string_lossy().into_owned());
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
    fn a_modern_jdk_case_compiles_against_its_own_jdk_and_names_it_to_the_reference() {
        let roots = roots().with_jdk_homes([(17, PathBuf::from("/jdks/17"))]);
        let selected = roots
            .for_modern_jdk(super::super::ModernJdk { release: 17 })
            .expect("JDK 17 is installed");
        assert_eq!(
            selected.classpath_root(),
            Some(Path::new("/jdks/17/lib/modules"))
        );
        assert_eq!(
            selected.kotlinc_args(&[]),
            Ok(vec!["-jdk-home".to_string(), "/jdks/17".to_string()])
        );
        assert_eq!(
            roots
                .for_modern_jdk(super::super::ModernJdk { release: 11 })
                .err(),
            Some("needs JDK 11 (set KRUSTY_JDK_11_HOME or JAVA_HOME_11_X64)".to_string())
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
