//! Physical class-file presence and first-owner queries.
//!
//! These operations bridge resolved classifier identities to classpath storage locations. Complete
//! catalogs answer without reading class bytes; incomplete entries retain the byte-verified path.

use std::path::PathBuf;

use super::{Classpath, Entry, JarId, PackageNode, PackageTree};
use crate::name_tree::{NameId, NameTree};
use crate::types::{existing_type_name_in, type_name, TypeName};

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
        self.stub_overlay.borrow().contains_key(&physical)
    }
}

impl PackageTree {
    /// The first classpath entry whose class file is stored as `internal`. The spelling is the
    /// zip entry without `.class`. A miss does not intern it.
    pub(super) fn first_jar_for_spelling(&self, internal: &str) -> Option<JarId> {
        self.jars_declaring_spelling(internal).next()
    }

    /// Every entry that stores a class file as `internal`, in classpath (shadowing) order.
    pub(super) fn jars_for_class(&self, internal: &str) -> Vec<JarId> {
        self.jars_declaring_spelling(internal).collect()
    }

    /// [`Self::jars_for_class`] for a resolved class identity.
    pub(super) fn jars_for_class_name(&self, internal: TypeName) -> Vec<JarId> {
        self.jars_declaring(internal.namespace(), move |names| {
            existing_type_name_in(names, internal)
        })
        .collect()
    }

    /// Whether `package` directly declares a class whose final path segment is `class_segment`.
    /// The segment is the class file's last component (`CollectionsKt`, `Map$Entry`), not a source
    /// nested name, and a miss does not intern it into the global type-name tree.
    pub(in crate::jvm) fn contains_exact_class(
        &self,
        package: TypeName,
        class_segment: &str,
    ) -> bool {
        self.jars_declaring(package, move |names| {
            names.existing_child_of(existing_type_name_in(names, package)?, class_segment)
        })
        .next()
        .is_some()
    }

    /// Whether `owner`'s flattened JVM nested class (`Owner$Companion`,
    /// `Outer$Inner$Companion`) is declared. The owner class file itself need not be present. Both
    /// probes stay inside the entry catalogs' name trees, so a miss neither formats nor interns a
    /// candidate.
    pub(in crate::jvm) fn contains_nested_class(&self, owner: TypeName, nested: &str) -> bool {
        let package = owner.namespace();
        self.jars_declaring(package, move |names| {
            names.existing_nested_under(
                existing_type_name_in(names, package)?,
                owner.segment_ref(),
                nested,
            )
        })
        .next()
        .is_some()
    }

    fn jars_declaring_spelling<'a>(
        &'a self,
        internal: &'a str,
    ) -> impl Iterator<Item = JarId> + 'a {
        let package = internal.rsplit_once('/').map_or("", |(package, _)| package);
        self.entries_declaring(self.node_for(package), move |names| names.get(internal))
    }

    fn jars_declaring<'a>(
        &'a self,
        package: TypeName,
        class: impl Fn(&NameTree) -> Option<NameId> + 'a,
    ) -> impl Iterator<Item = JarId> + 'a {
        self.entries_declaring(self.node_for_name(package), class)
    }

    /// The entries of `package`'s node, in classpath order, whose own catalog declares the class
    /// `class` finds in that catalog's name tree. Only the package is composed across entries; a
    /// class is answered by the entries that hold its package.
    fn entries_declaring<'a>(
        &'a self,
        package: Option<&'a PackageNode>,
        class: impl Fn(&NameTree) -> Option<NameId> + 'a,
    ) -> impl Iterator<Item = JarId> + 'a {
        package
            .into_iter()
            .flat_map(|node| node.jars.iter().copied())
            .filter(move |&jar| {
                self.parts
                    .get(jar)
                    .and_then(Option::as_ref)
                    .is_some_and(|part| {
                        class(&part.names).is_some_and(|id| part.declares_class(id))
                    })
            })
    }

    /// Every class the classpath declares, as its slashed internal name and the entry that owns it,
    /// entry by entry in classpath order. A name two entries declare is listed once for each, and
    /// the earlier entry's listing is the one resolution sees.
    pub fn classes(&self) -> impl Iterator<Item = (String, JarId)> + '_ {
        self.parts.iter().enumerate().flat_map(|(jar, part)| {
            part.iter().flat_map(move |part| {
                part.classes
                    .iter()
                    .map(move |&class| (part.names.render(class), jar))
            })
        })
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
            &|candidate: &str| candidate == "java/lang/Object",
        )
        .expect("stub");
        let cp = Classpath::new(vec![]);
        cp.set_stub_overlay(stubs);

        assert!(cp.stub_overlay_contains_classifier(type_name("p/Outer.Inner")));
        assert!(!cp.stub_overlay_contains_classifier(type_name("p/Outer.Missing")));
    }
}
