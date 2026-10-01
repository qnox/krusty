//! Physical class-file presence and first-owner queries.
//!
//! These operations bridge resolved classifier identities to classpath storage locations. Complete
//! catalogs answer without reading class bytes; incomplete entries retain the byte-verified path.

use std::path::PathBuf;

use super::{Classpath, Entry, JarId, PackageTree};
use crate::name_tree::NameId;
use crate::types::{type_name, TypeName};

impl Classpath {
    /// Whether the first classpath definition of `internal` belongs to a friend entry.
    /// A complete catalog already records that entry, so the check does not render the classifier
    /// or open its class bytes. An incomplete catalog still reads the entry to confirm ownership.
    pub fn grants_internal_access(&self, internal: TypeName) -> bool {
        let spelling = crate::jvm::names::classfile_internal_name_of(internal);
        if self.stub_overlay_contains_classifier(internal) {
            return false;
        }
        let tree = self.package_tree();
        if let Some(index) = tree.first_jar_for_spelling(spelling) {
            return self.friend_entries.get(index).copied().unwrap_or(false);
        }
        if tree.catalog_complete() {
            return false;
        }
        self.physical_class_entry(spelling)
            .and_then(|(index, _)| self.friend_entries.get(index))
            .copied()
            .unwrap_or(false)
    }

    /// Existence of an already-interned classifier. A complete catalog answers from the physical
    /// classfile spelling without promoting a miss into the global name tree. An incomplete catalog
    /// still probes the corresponding classfile bytes.
    pub(in crate::jvm) fn class_exists_name(&self, internal: TypeName) -> bool {
        let spelling = crate::jvm::names::classfile_internal_name_of(internal);
        if self.stub_overlay_contains_classifier(internal) {
            return true;
        }
        let tree = self.package_tree();
        if tree.first_jar_for_spelling(spelling).is_some() {
            return true;
        }
        if tree.incomplete_entries.is_empty() {
            return false;
        }
        self.class_exists(spelling)
    }

    /// Return the jar containing `internal`, if its first classpath definition is in a jar.
    /// A complete catalog answers from the classfile spelling, so the class bytes are not read.
    pub fn owning_jar(&self, internal: &str) -> Option<PathBuf> {
        let classifier = type_name(internal);
        let spelling = crate::jvm::names::classfile_internal_name_of(classifier);
        if self.stub_overlay_contains_classifier(classifier) {
            return None;
        }
        let tree = self.package_tree();
        let index = if let Some(index) = tree.first_jar_for_spelling(spelling) {
            index
        } else if tree.catalog_complete() {
            return None;
        } else {
            self.physical_class_entry(spelling)?.0
        };
        match self.entries.get(index)? {
            Entry::Jar(path) => Some(path.clone()),
            Entry::Dir(_) | Entry::Jimage(_) | Entry::CtSym { .. } => None,
        }
    }

    fn stub_overlay_contains_classifier(&self, classifier: TypeName) -> bool {
        let physical = super::super::jvm_class_map::to_jvm_classfile_type_name(classifier);
        let overlay = self.stub_overlay.borrow();
        if overlay.contains_key(&physical) {
            return true;
        }

        // Metadata can retain source-facing dots between nested classifier segments while the
        // parsed overlay records the physical `$` spelling. Probe that sibling directly in the
        // interned name tree instead of rendering and round-tripping through a textual lookup.
        let segment = physical.segment_ref();
        if !segment.contains('.') {
            return false;
        }
        let binary_segment = segment.replace('.', "$");
        physical
            .parent()
            .and_then(|namespace| {
                crate::types::existing_type_name_child(namespace, &binary_segment)
            })
            .is_some_and(|binary| overlay.contains_key(&binary))
    }
}

impl PackageTree {
    /// The first classpath entry whose class file is stored as `internal`. The spelling is the
    /// zip entry without `.class`. A miss does not intern it.
    pub(super) fn first_jar_for_spelling(&self, internal: &str) -> Option<JarId> {
        self.first_jar_for_id(self.names.get(internal)?)
    }

    pub(super) fn first_jar_for_id(&self, class: NameId) -> Option<JarId> {
        let start = self
            .classes
            .partition_point(|&(candidate, _)| candidate.0 < class.0);
        self.classes
            .get(start)
            .and_then(|&(candidate, jar)| (candidate == class).then_some(jar))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{test_temp_dir, write_test_archive_entries};
    use super::*;

    #[test]
    fn owning_jar_returns_the_jar_path_for_a_library_class() {
        let Some(jar) = crate::toolchain::stdlib_jar() else {
            return; // toolchain not provisioned
        };
        let cp = Classpath::new(vec![jar.clone()]);
        let owner = cp.owning_jar("kotlin/collections/CollectionsKt");
        assert_eq!(owner.as_deref(), Some(jar.as_path()));
        assert_eq!(
            cp.owning_jar("kotlin/Function1").as_deref(),
            Some(jar.as_path()),
            "Function1's bytes live in kotlin/jvm/functions/Function1.class"
        );
        let present = type_name("kotlin/collections/CollectionsKt");
        let absent = type_name("kotlin/collections/NoSuchKt");
        assert!(cp.class_exists_name(present));
        assert!(!cp.class_exists_name(absent));
        assert_eq!(
            cp.class_exists("kotlin/String"),
            cp.class_exists_name(type_name("kotlin/String"))
        );
        assert_eq!(
            cp.class_exists("kotlin/Function1"),
            cp.class_exists_name(type_name("kotlin/Function1"))
        );
    }

    // The catalog records the physical class-file name. `kotlin/Function1` and `kotlin/String`
    // are not those names, and the entry bytes are not a class; the jar is still the owner.
    #[test]
    fn owning_jar_finds_a_mapped_classfile_without_parsing_it() {
        let directory = test_temp_dir("owning-jar-classfile");
        std::fs::create_dir_all(&directory).expect("create temp dir");
        let jar = directory.join("mapped.jar");
        write_test_archive_entries(
            &jar,
            &[
                ("kotlin/jvm/functions/Function1.class", b"not-a-class"),
                ("java/lang/String.class", b"not-a-class"),
                ("probe/owner6044/Outer$Inner.class", b"not-a-class"),
            ],
        );
        let classpath = Classpath::new(vec![jar.clone()]);
        assert_eq!(
            classpath.owning_jar("kotlin/Function1").as_deref(),
            Some(jar.as_path())
        );
        assert_eq!(
            classpath.owning_jar("kotlin/String").as_deref(),
            Some(jar.as_path())
        );
        let nested = crate::types::type_name_child(type_name("probe/owner6044"), "Outer.Inner");
        assert!(classpath.class_exists_name(nested));
        assert_eq!(
            classpath
                .owning_jar("probe/owner6044/Outer.Inner")
                .as_deref(),
            Some(jar.as_path())
        );
        assert!(classpath.owning_jar("kotlin/NoSuch").is_none());
        assert!(
            classpath.find_name(type_name("kotlin/Function1")).is_none(),
            "the metadata name is not the catalog key of the physical class"
        );

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove temp dir");
    }

    #[test]
    fn stub_overlay_matches_a_nested_classifier_without_a_spelling_round_trip() {
        let stubs = crate::jvm::java_stub::stub_classes(
            &[(
                "Outer.java".into(),
                "package p; public class Outer { public static class Inner {} }".into(),
            )],
            crate::jvm::java_stub::StubMode::Lenient,
            &|candidate| candidate == "java/lang/Object",
        )
        .expect("stub");
        let cp = Classpath::new(vec![]);
        cp.set_stub_overlay(stubs);

        assert!(cp.stub_overlay_contains_classifier(type_name("p/Outer.Inner")));
        assert!(!cp.stub_overlay_contains_classifier(type_name("p/Outer.Missing")));
    }
}
