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

use super::{directive, directive_payload};

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
    /// The installed JDK homes by feature release, for cases that name a full JDK.
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
    /// case that names a full JDK of one feature release (`JDK_KIND: FULL_JDK_<N>`, or a
    /// `boxModernJdk` case), which runs on that JDK.
    Home {
        /// The JDK's feature release.
        release: u16,
        home: &'a Path,
        /// Its bootclasspath root (`lib/modules`, or `jre/lib/rt.jar` on JDK 8).
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

    /// The installed JDK homes by feature release, which [`BoxJdkRoots::for_release`] reads.
    pub fn with_jdk_homes(mut self, homes: impl IntoIterator<Item = (u16, PathBuf)>) -> Self {
        self.homes = homes
            .into_iter()
            .map(|(release, home)| {
                // JDK 9 and later keep the bootclasspath in the `lib/modules` jimage; JDK 8 in
                // `jre/lib/rt.jar`.
                let modules = home.join("lib").join("modules");
                let rt_jar = home.join("jre").join("lib").join("rt.jar");
                let modules = if rt_jar.is_file() && !modules.is_file() {
                    rt_jar
                } else {
                    modules
                };
                (release, (home, modules))
            })
            .collect();
        self
    }

    /// The installed JDK of one feature release. A release with no installed JDK is an error,
    /// never a reason to use another one: the JDK surface changes member resolution.
    pub fn for_release(&self, release: u16) -> Result<BoxJdk<'_>, String> {
        let (home, modules) = self.homes.get(&release).ok_or_else(|| {
            format!(
                "needs JDK {release} (set KRUSTY_JDK_{release}_HOME or JAVA_HOME_{release}_X64)"
            )
        })?;
        Ok(BoxJdk::Home {
            release,
            home,
            modules,
        })
    }

    /// The JDK a case selects with `// JDK_KIND:`, as kotlinc's `JvmEnvironmentConfigurator`
    /// resolves it: `FULL_JDK_<N>` is that release's installed JDK (kotlinc puts its home in
    /// `JDK_HOME`, whatever `JDK_RELEASE` says), `FULL_JDK` is the JDK the harness runs on, and a
    /// case without `JDK_KIND` falls to [`BoxJdkRoots::select`]. The Valhalla kind has no JDK to
    /// install here and fails; so does an unknown kind.
    pub fn for_jdk_kind(&self, src: &str) -> Result<BoxJdk<'_>, String> {
        let Some(kind) = src
            .lines()
            .find_map(|line| directive_payload(line, "JDK_KIND"))
            .map(str::trim)
        else {
            return Ok(self.select(src));
        };
        match kind {
            "FULL_JDK" => Ok(BoxJdk::Full {
                root: self.full_jdk.as_deref(),
            }),
            // The mock kinds leave the box corpus's mock JDK selection in place.
            "MOCK_JDK" | "MODIFIED_MOCK_JDK" => Ok(self.select(src)),
            other => match other
                .strip_prefix("FULL_JDK_")
                .and_then(|release| release.parse::<u16>().ok())
            {
                Some(release) => self.for_release(release),
                None => Err(format!("JDK_KIND: {other} names no installed JDK")),
            },
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
            BoxJdk::Home { modules, .. } => Some(modules),
        }
    }

    /// The JDK installation and feature release a case runs on, when the case names one.
    pub fn home(self) -> Option<(&'a Path, u16)> {
        match self {
            BoxJdk::Home { home, release, .. } => Some((home, release)),
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
    fn a_full_jdk_release_compiles_against_its_own_jdk_and_names_it_to_the_reference() {
        let roots = roots().with_jdk_homes([(17, PathBuf::from("/jdks/17"))]);
        let selected = roots.for_release(17).expect("JDK 17 is installed");
        assert_eq!(
            selected.classpath_root(),
            Some(Path::new("/jdks/17/lib/modules"))
        );
        assert_eq!(
            selected.kotlinc_args(&[]),
            Ok(vec!["-jdk-home".to_string(), "/jdks/17".to_string()])
        );
        assert_eq!(
            roots.for_release(11).err(),
            Some("needs JDK 11 (set KRUSTY_JDK_11_HOME or JAVA_HOME_11_X64)".to_string())
        );
    }

    #[test]
    fn a_jdk_kind_selects_its_jdk_like_kotlincs_configurator() {
        let roots = roots().with_jdk_homes([(11, PathBuf::from("/jdks/11"))]);
        let case = |kind: &str| format!("// JDK_KIND: {kind}\nfun box() = \"OK\"\n");
        assert_eq!(
            roots.for_jdk_kind(&case("FULL_JDK_11")).map(BoxJdk::home),
            Ok(Some((Path::new("/jdks/11"), 11)))
        );
        assert_eq!(
            roots.for_jdk_kind(&case("FULL_JDK")),
            Ok(BoxJdk::Full {
                root: Some(Path::new("/jdk/lib/modules"))
            })
        );
        assert_eq!(
            roots.for_jdk_kind(&case("FULL_JDK_17")).err(),
            Some("needs JDK 17 (set KRUSTY_JDK_17_HOME or JAVA_HOME_17_X64)".to_string())
        );
        assert_eq!(
            roots.for_jdk_kind(&case("FULL_JDK_VALHALLA")).err(),
            Some("JDK_KIND: FULL_JDK_VALHALLA names no installed JDK".to_string())
        );
        assert_eq!(
            roots.for_jdk_kind("fun box() = \"OK\"\n"),
            Ok(roots.select("fun box() = \"OK\"\n"))
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
