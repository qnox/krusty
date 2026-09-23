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
    } else if path.is_file()
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
    /// image is appended to the explicit compiler classpath instead.
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
            if classify_classpath_entry(entry, None) != JvmClasspathEntryKind::Archive {
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

pub(crate) fn selected_jdk_modules(jdk_home: Option<&Path>) -> Option<PathBuf> {
    let base = jdk_home
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("JAVA_HOME").map(PathBuf::from))?;
    let modules = base.join("lib").join("modules");
    modules.is_file().then_some(modules)
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
}
