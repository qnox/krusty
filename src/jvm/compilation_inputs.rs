//! Exact filesystem inputs selected by JVM compilation outside the source list.
//!
//! The CLI, classpath provider, and build cache must agree on this inventory. Discovery lives here
//! so a cache adapter never duplicates a toolchain filename convention and a provider never reads a
//! sibling file that the cache key cannot name.

use std::path::{Path, PathBuf};

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

/// Exact JVM classpath roots plus target-owned files selected implicitly from them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JvmCompilationInputInventory {
    effective_classpath: Vec<PathBuf>,
    /// The selected JDK bootclasspath root: the `lib/modules` jimage on JDK 9+, or the
    /// `jre/lib/rt.jar` archive on JDK 8 and earlier (see [`selected_jdk_modules`]). The field
    /// name keeps the historical "modules" spelling, but the value is deliberately widened at
    /// the SELECTION boundary rather than the reading boundary: a pre-jimage `rt.jar` is an
    /// ordinary archive classpath entry to the provider, so no jimage concept widens and the
    /// resolver never learns a JDK layout.
    jdk_modules: Option<PathBuf>,
    common_expectation_klib: Option<PathBuf>,
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
        let jdk_modules = selected_jdk_modules(jdk_home);
        let mut effective_classpath = classpath.to_vec();
        effective_classpath.extend(jdk_modules.iter().cloned());
        Self::from_classpath_and_jdk(effective_classpath, jdk_modules)
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
            common_expectation_klib,
        }
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
            .chain(self.common_expectation_klib())
    }
}

/// Select the JDK's bootclasspath root from a home: the `lib/modules` jimage on JDK 9+, falling
/// back to the `jre/lib/rt.jar` archive on JDK 8 and earlier, which have no jimage. The fallback
/// is a selection, not a new container kind: `rt.jar` is classified and read as an ordinary
/// archive classpath entry, so every consumer (provider, build cache, LSP) treats both roots
/// identically.
pub(crate) fn selected_jdk_modules(jdk_home: Option<&Path>) -> Option<PathBuf> {
    let base = jdk_home
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("JAVA_HOME").map(PathBuf::from))?;
    let modules = base.join("lib").join("modules");
    if modules.is_file() {
        return Some(modules);
    }
    let rt_jar = base.join("jre").join("lib").join("rt.jar");
    rt_jar.is_file().then_some(rt_jar)
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
}
