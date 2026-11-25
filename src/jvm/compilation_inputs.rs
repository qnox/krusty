//! Exact filesystem inputs selected by JVM compilation outside the source list.
//!
//! The CLI, classpath provider, and build cache must agree on this inventory. Discovery lives here
//! so a cache adapter never duplicates a toolchain filename convention and a provider never reads a
//! sibling file that the cache key cannot name.

use std::path::{Path, PathBuf};

/// The environment variable that turns off per-class `@Metadata` in shipping output.
pub const NO_CLASS_METADATA_VARIABLE: &str = "KRUSTY_NO_CLASS_METADATA";

/// The environment variables a JVM compilation's output depends on, beyond the files it reads. A
/// build key must include each one's presence and value. Variables that only steer diagnostics,
/// tracing or caches do not belong here.
pub const OUTPUT_ENVIRONMENT: [&str; 2] = [
    NO_CLASS_METADATA_VARIABLE,
    crate::kotlin_version::REFERENCE_VERSION_VARIABLE,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JvmClasspathEntryKind {
    Archive,
    JdkImage,
    CtSym,
    Directory,
}

pub(crate) fn classify_classpath_entry(
    path: &Path,
    jdk_release: Option<u8>,
) -> JvmClasspathEntryKind {
    if path.is_file() && path.file_name().is_some_and(|name| name == "modules") {
        JvmClasspathEntryKind::JdkImage
    } else if jdk_release.is_some()
        && path.is_file()
        && path.file_name().is_some_and(|name| name == "ct.sym")
    {
        JvmClasspathEntryKind::CtSym
    } else if !path.is_dir()
        // The extension names an archive; only an actual directory overrides it. A path that does
        // not exist yet keeps its archive kind, as it always did — what that path shares (the
        // process-global inline-plan cache) must not change with whether the file is present.
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("jar") || extension.eq_ignore_ascii_case("zip")
            })
    {
        JvmClasspathEntryKind::Archive
    } else {
        JvmClasspathEntryKind::Directory
    }
}

/// Whether the compilation reads the classpath entry at `path` as a zip archive (a jar), as it
/// reads it with no JDK release selected. A build key that reads an archive's entries the way the
/// compilation does asks here rather than repeating the convention.
pub fn is_archive_entry(path: &Path) -> bool {
    classify_classpath_entry(path, None) == JvmClasspathEntryKind::Archive
}

/// Exact JVM classpath roots plus target-owned files selected implicitly from them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JvmCompilationInputInventory {
    effective_classpath: Vec<PathBuf>,
    /// The selected JDK bootclasspath root: the `lib/modules` jimage on JDK 9+, or the
    /// `jre/lib/rt.jar` archive on JDK 8 and earlier (see [`selected_jdk_modules`]). The field
    /// name keeps the historical "modules" spelling, but the value is deliberately widened at
    /// the SELECTION boundary rather than the reading boundary: a pre-jimage `rt.jar` is an
    /// ordinary archive classpath entry to the provider, so no jimage concept widens and the
    /// resolver never learns a JDK layout. For an `-Xjdk-release` other than the JDK's own it
    /// is the JDK's `lib/ct.sym`, read as the view of `ct_sym_release`.
    jdk_modules: Option<PathBuf>,
    /// The release whose view of `jdk_modules` the compilation reads when that root is the JDK's
    /// `lib/ct.sym` (see [`JdkApi::for_release`]); `None` for the JDK's own API.
    ct_sym_release: Option<u8>,
    /// The selected pre-jimage JDK's other runtime archives (see [`JdkApi`]).
    jdk_companions: Vec<PathBuf>,
    common_expectation_klib: Option<PathBuf>,
}

/// The JDK API a compilation compiles against: a bootclasspath root, and the release view to read
/// when that root is the JDK's `lib/ct.sym`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JdkApi {
    root: PathBuf,
    ct_sym_release: Option<u8>,
    /// The runtime archives a pre-jimage JDK keeps beside `rt.jar` (`jre/lib/*.jar` and
    /// `jre/lib/ext/*.jar`), which kotlinc reads as part of the JDK's classes. Empty for a JDK
    /// with a module image and for a `ct.sym` release view.
    companions: Vec<PathBuf>,
}

impl JdkApi {
    /// The selected JDK's own API: its module image, or `rt.jar` before JDK 9.
    fn own(jdk_home: Option<&Path>) -> Option<Self> {
        let root = selected_jdk_modules(jdk_home)?;
        let companions = pre_jimage_companions(&root);
        Some(JdkApi {
            root,
            ct_sym_release: None,
            companions,
        })
    }

    /// Select the JDK API for `-Xjdk-release=<release>` as kotlinc's `CliJavaModuleFinder` does.
    /// The JDK's `lib/ct.sym` is required and names the releases it describes; the newest of them
    /// is the JDK's own release, compiled against its own modules. Any other release it describes
    /// is read from that release's signatures in `ct.sym`. A release it does not describe is an
    /// error, as is a JDK without `ct.sym`. A release from 9 on is modular, so either error leaves
    /// it without `java.base`, which kotlinc reports first. A release with no JDK to read, or the
    /// JDK's own release in a home with no bootclasspath root, is a configuration error too (kotlinc
    /// always runs on a JDK, and fails with an internal error on such a home). With no release,
    /// the JDK's own API is selected, or nothing when there is none.
    pub fn for_release(
        jdk_home: Option<&Path>,
        release: Option<u16>,
    ) -> Result<Option<JdkApi>, Vec<String>> {
        let Some(release) = release else {
            return Ok(JdkApi::own(jdk_home));
        };
        let Some(home) = selected_jdk_home(jdk_home) else {
            return Err(vec![format!(
                "-Xjdk-release={release} needs a JDK: pass -jdk-home or set JAVA_HOME"
            )]);
        };
        let unavailable = |reason: String| {
            let mut errors = Vec::new();
            if release >= 9 {
                errors.push("module java.base cannot be found in the module graph".to_string());
            }
            errors.push(reason);
            errors
        };
        let symbols = home.join("lib").join("ct.sym");
        let releases = if symbols.is_file() {
            crate::jvm::classpath::ct_sym_index::ct_sym_releases(&symbols)
                .map_err(|error| vec![error])?
        } else {
            None
        };
        let Some(releases) = releases else {
            return Err(unavailable(format!(
                "this JDK does not have the 'ct.sym' file required for the '-Xjdk-release={release}' \
                 option: {}",
                home.display()
            )));
        };
        if releases
            .last()
            .is_some_and(|&own| u16::from(own) == release)
        {
            return match JdkApi::own(Some(&home)) {
                Some(own) => Ok(Some(own)),
                None => Err(vec![format!(
                    "-Xjdk-release={release} is the JDK's own release, but {} has neither \
                     lib/modules nor jre/lib/rt.jar",
                    home.display()
                )]),
            };
        }
        match u8::try_from(release)
            .ok()
            .filter(|release| releases.contains(release))
        {
            Some(release) => Ok(Some(JdkApi {
                root: symbols,
                ct_sym_release: Some(release),
                companions: Vec::new(),
            })),
            None => Err(unavailable(format!(
                "'-Xjdk-release={release}' option is not supported by used JDK: {}",
                home.display()
            ))),
        }
    }
}

impl JvmCompilationInputInventory {
    /// Record an already-effective classpath and inventory companion inputs selected from it.
    pub fn from_effective_classpath(classpath: &[PathBuf]) -> Self {
        Self::from_classpath_and_jdk(classpath.to_vec(), None)
    }

    /// Inventory inputs selected when an explicit classpath is combined with the configured JDK.
    ///
    /// This is the boundary used before `-no-jdk` turns ambient JDK discovery off and the selected
    /// JDK bootclasspath root is appended to the explicit compiler classpath instead.
    pub fn from_explicit_classpath_and_jdk(classpath: &[PathBuf], jdk_home: Option<&Path>) -> Self {
        Self::from_explicit_classpath_and_jdk_api(classpath, JdkApi::own(jdk_home))
    }

    /// Inventory inputs selected when an explicit classpath is combined with the JDK API that
    /// [`JdkApi::for_release`] selected.
    pub fn from_explicit_classpath_and_jdk_api(classpath: &[PathBuf], api: Option<JdkApi>) -> Self {
        let mut effective_classpath = classpath.to_vec();
        if let Some(api) = &api {
            effective_classpath.push(api.root.clone());
            effective_classpath.extend(api.companions.iter().cloned());
        }
        let ct_sym_release = api.as_ref().and_then(|api| api.ct_sym_release);
        let companions = api
            .as_ref()
            .map(|api| api.companions.clone())
            .unwrap_or_default();
        let mut inventory =
            Self::from_classpath_and_jdk(effective_classpath, api.map(|api| api.root));
        inventory.ct_sym_release = ct_sym_release;
        inventory.jdk_companions = companions;
        inventory
    }

    fn from_classpath_and_jdk(
        effective_classpath: Vec<PathBuf>,
        jdk_modules: Option<PathBuf>,
    ) -> Self {
        let common_expectation_klib = effective_classpath.iter().find_map(|entry| {
            if !entry.is_file()
                || classify_classpath_entry(entry, None) != JvmClasspathEntryKind::Archive
            {
                return None;
            }
            let file_name = entry.file_name()?.to_str()?;
            if file_name != "kotlin-stdlib.jar" {
                return None;
            }
            let klib = entry.parent()?.join("kotlin-stdlib-wasm-js.klib");
            klib.exists().then_some(klib)
        });
        Self {
            effective_classpath,
            jdk_modules,
            ct_sym_release: None,
            jdk_companions: Vec::new(),
            common_expectation_klib,
        }
    }

    /// The release whose view of the JDK's `lib/ct.sym` the classpath reads, when the selected JDK
    /// root is that archive rather than the JDK's own API. The provider is built with it.
    pub fn ct_sym_release(&self) -> Option<u8> {
        self.ct_sym_release
    }

    /// Classpath roots the CLI hands to the JVM provider, in exact lookup order.
    pub fn effective_classpath(&self) -> &[PathBuf] {
        &self.effective_classpath
    }

    /// The selected JDK bootclasspath root, if the home offered one (`lib/modules`, or
    /// `jre/lib/rt.jar` on a pre-jimage JDK).
    pub fn jdk_modules(&self) -> Option<&Path> {
        self.jdk_modules.as_deref()
    }

    pub fn common_expectation_klib(&self) -> Option<&Path> {
        self.common_expectation_klib.as_deref()
    }

    /// Files read in addition to the explicit classpath. Their contents must be part of a build key.
    pub fn implicit_files(&self) -> impl Iterator<Item = &Path> {
        self.jdk_modules()
            .into_iter()
            .chain(self.jdk_companions.iter().map(PathBuf::as_path))
            .chain(self.common_expectation_klib())
    }
}

/// The JDK compilation reads: `-jdk-home`, else `JAVA_HOME`.
fn selected_jdk_home(jdk_home: Option<&Path>) -> Option<PathBuf> {
    jdk_home
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("JAVA_HOME").map(PathBuf::from))
}

/// The feature release of the selected JDK, read from its `release` file:
/// `JAVA_VERSION="25.0.4.1"` is 25 and `JAVA_VERSION="1.8.0_402"` is 8. `None` when the home has
/// no readable `release` file or the version does not parse.
pub fn selected_jdk_feature_release(jdk_home: Option<&Path>) -> Option<u16> {
    let release = std::fs::read_to_string(selected_jdk_home(jdk_home)?.join("release")).ok()?;
    let version = release.lines().find_map(|line| {
        line.strip_prefix("JAVA_VERSION=")
            .map(|value| value.trim().trim_matches('"'))
    })?;
    let version = version.strip_prefix("1.").unwrap_or(version);
    let feature = version
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .filter(|digits| !digits.is_empty())?;
    feature.parse().ok()
}

/// Select the JDK's bootclasspath root from a home: the `lib/modules` jimage on JDK 9+, falling
/// back to the `jre/lib/rt.jar` archive on JDK 8 and earlier, which have no jimage. The fallback
/// is a selection, not a new container kind: `rt.jar` is classified and read as an ordinary
/// archive classpath entry, so every consumer (provider, build cache, LSP) treats both roots
/// identically.
pub(crate) fn selected_jdk_modules(jdk_home: Option<&Path>) -> Option<PathBuf> {
    let base = selected_jdk_home(jdk_home)?;
    let modules = base.join("lib").join("modules");
    if modules.is_file() {
        return Some(modules);
    }
    let rt_jar = base.join("jre").join("lib").join("rt.jar");
    rt_jar.is_file().then_some(rt_jar)
}

/// The runtime archives of a pre-jimage JDK besides `rt.jar`: the other jars of `jre/lib`, then
/// those of `jre/lib/ext`, each directory in file-name order. Empty when `root` is not a
/// `jre/lib/rt.jar`.
fn pre_jimage_companions(root: &Path) -> Vec<PathBuf> {
    if root.file_name().is_none_or(|name| name != "rt.jar") {
        return Vec::new();
    }
    let Some(lib) = root.parent() else {
        return Vec::new();
    };
    let jars_in = |directory: PathBuf| {
        let mut jars: Vec<PathBuf> = std::fs::read_dir(directory)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
            })
            .collect();
        jars.sort();
        jars
    };
    let mut companions = jars_in(lib.to_path_buf());
    companions.retain(|path| path != root);
    companions.extend(jars_in(lib.join("ext")));
    companions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classpath::Classpath;

    #[test]
    fn inventory_matches_the_classpath_providers_actual_companion_input() {
        let root = std::env::temp_dir().join(format!(
            "krusty-jvm-input-inventory-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("mkdir");
        let stdlib = root.join("kotlin-stdlib.jar");
        let common = root.join("kotlin-stdlib-wasm-js.klib");
        std::fs::write(&stdlib, b"not opened by this selection test").expect("stdlib");
        std::fs::write(&common, b"common metadata").expect("common");

        let paths = vec![stdlib];
        let inventory = JvmCompilationInputInventory::from_effective_classpath(&paths);
        assert_eq!(inventory.effective_classpath(), paths);
        let actual = Classpath::new(paths).common_expectation_klib();
        assert_eq!(inventory.common_expectation_klib(), Some(common.as_path()));
        assert_eq!(actual.as_deref(), inventory.common_expectation_klib());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_filename_alone_does_not_turn_a_directory_or_missing_path_into_stdlib() {
        let root = std::env::temp_dir().join(format!(
            "krusty-jvm-input-kind-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("kotlin-stdlib.jar")).expect("jar-named directory");
        std::fs::write(root.join("kotlin-stdlib-wasm-js.klib"), b"common").expect("common");
        for path in [
            root.join("kotlin-stdlib.jar"),
            root.join("missing/kotlin-stdlib.jar"),
        ] {
            let paths = vec![path];
            let inventory = JvmCompilationInputInventory::from_effective_classpath(&paths);
            let actual = Classpath::new(paths).common_expectation_klib();
            assert_eq!(inventory.common_expectation_klib(), None);
            assert_eq!(actual, None);
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn inventory_matches_the_cli_jdk_selection_contract() {
        let root =
            std::env::temp_dir().join(format!("krusty-jvm-jdk-inventory-{}", std::process::id()));
        std::fs::create_dir_all(root.join("lib")).expect("mkdir");
        std::fs::write(root.join("lib/modules"), b"jimage").expect("modules");

        let inventory =
            JvmCompilationInputInventory::from_explicit_classpath_and_jdk(&[], Some(&root));
        let selected = crate::jvm::classpath::platform_jdk_modules(Some(&root));
        assert_eq!(inventory.jdk_modules(), selected.as_deref());
        assert_eq!(
            inventory.effective_classpath(),
            [root.join("lib/modules")],
            "the inventory exposes the exact provider roots, including the selected JDK image"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A real archive carrying one real class, so a selection test proves the provider can READ
    /// what the inventory pointed at, not merely name it.
    fn write_class_archive(path: &Path, entry_name: &str, bytes: &[u8]) {
        let file = std::fs::File::create(path).expect("create archive");
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        archive
            .start_file(entry_name, options)
            .expect("start entry");
        std::io::Write::write_all(&mut archive, bytes).expect("write entry");
        archive.finish().expect("finish archive");
    }

    /// kotlinc reads the other runtime archives of a JDK 8 home (`charsets.jar`, `jce.jar`, the
    /// `ext` jars) with `rt.jar`: `jre/lib` first, then `jre/lib/ext`, each in file-name order. A
    /// JDK with a module image has none, and neither has a `ct.sym` release view.
    #[test]
    fn a_pre_jimage_jdk_home_reads_its_other_runtime_archives() {
        let root = std::env::temp_dir().join(format!(
            "krusty-jvm-jdk8-companions-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let lib = root.join("jre/lib");
        std::fs::create_dir_all(lib.join("ext")).expect("mkdir");
        for name in [
            "rt.jar",
            "jce.jar",
            "charsets.jar",
            "ext/sunjce_provider.jar",
            "ext/dnsns.jar",
            "ext/meta-index",
            "classlist",
        ] {
            std::fs::write(lib.join(name), b"x").expect("write");
        }
        let inventory =
            JvmCompilationInputInventory::from_explicit_classpath_and_jdk(&[], Some(&root));
        let expected = [
            "rt.jar",
            "charsets.jar",
            "jce.jar",
            "ext/dnsns.jar",
            "ext/sunjce_provider.jar",
        ]
        .map(|name| lib.join(name));
        assert_eq!(inventory.effective_classpath(), expected);
        assert_eq!(
            inventory.implicit_files().collect::<Vec<_>>(),
            expected.iter().map(PathBuf::as_path).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A JDK 8 home has no `lib/modules` jimage; its bootclasspath is `jre/lib/rt.jar`. The
    /// inventory must select that archive as the JDK root, and the classpath provider must
    /// resolve its (major-52) classes through the ordinary archive path.
    #[test]
    fn a_pre_jimage_jdk_home_resolves_its_rt_jar() {
        let root = std::env::temp_dir().join(format!(
            "krusty-jvm-jdk8-inventory-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("jre/lib")).expect("mkdir");
        let rt_jar = root.join("jre/lib/rt.jar");
        let mut writer = crate::jvm::classfile::ClassWriter::new("demo/Legacy", "java/lang/Object");
        assert_eq!(writer.major(), 52, "the fixture is the Java 8 class shape");
        writer.add_abstract_method(crate::jvm::classreader::ACC_PUBLIC, "legacy", "()V");
        write_class_archive(&rt_jar, "demo/Legacy.class", &writer.finish());

        let inventory =
            JvmCompilationInputInventory::from_explicit_classpath_and_jdk(&[], Some(&root));
        assert_eq!(
            inventory.jdk_modules(),
            Some(rt_jar.as_path()),
            "a home without lib/modules contributes its jre/lib/rt.jar"
        );
        assert_eq!(
            crate::jvm::classpath::platform_jdk_modules(Some(&root)).as_deref(),
            inventory.jdk_modules(),
            "the provider-facing selection must agree with the inventory"
        );
        assert_eq!(
            inventory.effective_classpath(),
            std::slice::from_ref(&rt_jar)
        );
        assert_eq!(
            inventory.implicit_files().collect::<Vec<_>>(),
            [rt_jar.as_path()],
            "the archive is read in addition to the explicit classpath, so it is a keyed input"
        );

        let classpath = Classpath::new(inventory.effective_classpath().to_vec());
        let class = classpath
            .find("demo/Legacy")
            .expect("the rt.jar entry resolves through the ordinary archive path");
        assert!(class.this_class_matches("demo/Legacy"));
        assert_eq!(class.major, 52, "the Java 8 (major 52) class file is read");
        assert!(
            class.method("legacy", "()V").is_some(),
            "the class's declarations are visible, not just its name"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A JDK that carries both layouts (an unsupported hybrid, but the inventory must pick one
    /// deterministically) resolves the jimage: it is the complete module view, while `rt.jar`
    /// is the pre-jimage fallback.
    #[test]
    fn the_jimage_is_preferred_over_rt_jar_when_both_exist() {
        let root = std::env::temp_dir().join(format!(
            "krusty-jvm-jdk-hybrid-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("lib")).expect("mkdir");
        std::fs::create_dir_all(root.join("jre/lib")).expect("mkdir");
        std::fs::write(root.join("lib/modules"), b"jimage").expect("modules");
        std::fs::write(root.join("jre/lib/rt.jar"), b"archive").expect("rt.jar");

        let inventory =
            JvmCompilationInputInventory::from_explicit_classpath_and_jdk(&[], Some(&root));
        assert_eq!(
            inventory.effective_classpath(),
            [root.join("lib/modules")],
            "the jimage wins and rt.jar is not ALSO added"
        );
        assert_eq!(
            inventory.jdk_modules(),
            Some(root.join("lib/modules").as_path())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_jdk_home_with_neither_boot_classpath_form_contributes_nothing() {
        let root = std::env::temp_dir().join(format!(
            "krusty-jvm-jdk-empty-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("lib")).expect("mkdir");
        std::fs::create_dir_all(root.join("jre/lib")).expect("mkdir");

        let explicit = vec![PathBuf::from("user.jar")];
        let inventory =
            JvmCompilationInputInventory::from_explicit_classpath_and_jdk(&explicit, Some(&root));
        assert_eq!(inventory.jdk_modules(), None);
        assert_eq!(
            inventory.effective_classpath(),
            explicit,
            "an unrecognized JDK home must not invent classpath entries"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// `-Xjdk-release` selects from the releases `ct.sym` names: its newest is the JDK's own and
    /// reads the module image, an older one reads that release's signatures, and one it does not
    /// name, or a JDK without `ct.sym`, is reported in kotlinc's words.
    #[test]
    fn a_jdk_release_selects_the_matching_ct_sym_view() {
        let root = std::env::temp_dir().join(format!(
            "krusty-jvm-jdk-release-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("lib")).expect("mkdir");
        let modules = root.join("lib/modules");
        std::fs::write(&modules, b"jimage").expect("modules");
        let home = root.display().to_string();
        let missing = |release: u16| {
            format!(
                "this JDK does not have the 'ct.sym' file required for the \
                 '-Xjdk-release={release}' option: {home}"
            )
        };
        assert_eq!(
            JdkApi::for_release(Some(&root), Some(8)),
            Err(vec![missing(8)])
        );
        assert_eq!(
            JdkApi::for_release(Some(&root), Some(11)),
            Err(vec![
                "module java.base cannot be found in the module graph".to_string(),
                missing(11)
            ])
        );

        let symbols = root.join("lib/ct.sym");
        let mut writer = crate::jvm::classfile::ClassWriter::new("demo/Legacy", "java/lang/Object");
        writer.add_abstract_method(crate::jvm::classreader::ACC_PUBLIC, "legacy", "()V");
        let file = std::fs::File::create(&symbols).expect("create ct.sym");
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let class = writer.finish();
        for (name, bytes) in [
            ("8/java.base/demo/Legacy.sig", class.as_slice()),
            ("9A/java.base/demo/Modular.sig", b"unread".as_slice()),
            ("C/system-modules", b"java.base".as_slice()),
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0".as_slice()),
        ] {
            archive.start_file(name, options).expect("start entry");
            std::io::Write::write_all(&mut archive, bytes).expect("write entry");
        }
        archive.finish().expect("finish ct.sym");

        let own = JdkApi {
            root: modules.clone(),
            ct_sym_release: None,
            companions: Vec::new(),
        };
        assert_eq!(
            JdkApi::for_release(Some(&root), None),
            Ok(Some(own.clone()))
        );
        assert_eq!(JdkApi::for_release(Some(&root), Some(12)), Ok(Some(own)));
        for release in [8, 9, 10] {
            assert_eq!(
                JdkApi::for_release(Some(&root), Some(release)),
                Ok(Some(JdkApi {
                    root: symbols.clone(),
                    ct_sym_release: Some(release as u8),
                    companions: Vec::new(),
                })),
                "release {release}"
            );
        }
        let not_supported = |release: u16| {
            format!("'-Xjdk-release={release}' option is not supported by used JDK: {home}")
        };
        assert_eq!(
            JdkApi::for_release(Some(&root), Some(7)),
            Err(vec![not_supported(7)])
        );
        assert_eq!(
            JdkApi::for_release(Some(&root), Some(11)),
            Err(vec![
                "module java.base cannot be found in the module graph".to_string(),
                not_supported(11)
            ])
        );

        let api = JdkApi::for_release(Some(&root), Some(8)).expect("release 8");
        let inventory = JvmCompilationInputInventory::from_explicit_classpath_and_jdk_api(&[], api);
        assert_eq!(
            inventory.effective_classpath(),
            std::slice::from_ref(&symbols)
        );
        assert_eq!(
            inventory.implicit_files().collect::<Vec<_>>(),
            [symbols.as_path()]
        );
        assert_eq!(inventory.ct_sym_release(), Some(8));
        let classpath = Classpath::new_with_friend_paths_and_jdk_release(
            inventory.effective_classpath().to_vec(),
            Vec::new(),
            inventory.ct_sym_release(),
        );
        let class = classpath
            .find("demo/Legacy")
            .expect("the release 8 signature resolves");
        assert!(class.method("legacy", "()V").is_some());
        let _ = std::fs::remove_dir_all(root);
    }
}
