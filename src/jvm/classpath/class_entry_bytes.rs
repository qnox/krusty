//! First-entry class-file bytes shared by declaration queries, locations and compiled bodies.

use super::{Classpath, Entry};
use crate::jvm::classreader::parse_class;

struct ClassEntryBytes<'a> {
    classpath: &'a Classpath,
}

impl ClassEntryBytes<'_> {
    fn mapped(&self, internal: &str) -> Option<Vec<u8>> {
        let internal = crate::jvm::jvm_class_map::to_jvm_internal(internal);
        self.physical(internal).map(|(_, bytes)| bytes)
    }

    fn physical(&self, internal: &str) -> Option<(usize, Vec<u8>)> {
        let name = format!("{internal}.class");
        let tree = self.classpath.package_tree();
        for index in self.classpath.class_entry_indices(&tree, internal) {
            let entry = self.classpath.entries.get(index)?;
            let bytes = match entry {
                Entry::Dir(directory) => {
                    std::fs::read(directory.join(&name)).ok().filter(|bytes| {
                        // A case-insensitive filesystem must not serve a case-collided sibling.
                        parse_class(bytes).is_ok_and(|class| class.this_class_matches(internal))
                    })
                }
                Entry::Jar(jar) => self.classpath.jar_entry(jar, &name),
                Entry::Jimage(_) => self.classpath.jimage_bytes(internal),
                Entry::CtSym { path, release } => {
                    self.classpath.ct_sym_bytes(path, *release, internal)
                }
            };
            if let Some(bytes) = bytes {
                return Some((index, bytes));
            }
        }
        None
    }
}

impl Classpath {
    /// Raw `.class` bytes with Kotlin built-in names mapped to JVM names first. Unlike `find`,
    /// this retains the bytes needed by the inline expander.
    pub(super) fn class_bytes(&self, internal: &str) -> Option<Vec<u8>> {
        ClassEntryBytes { classpath: self }.mapped(internal)
    }

    pub(super) fn physical_class_entry(&self, internal: &str) -> Option<(usize, Vec<u8>)> {
        ClassEntryBytes { classpath: self }.physical(internal)
    }
}
