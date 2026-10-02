//! Entry-local caches for decoded JVM method bodies and their shared class indexes.

use super::{Classpath, Entry, EntryCache, EntryKey};
use crate::jvm::classreader::{parse_class, ClassBodies, MethodCode};
use crate::types::TypeName;
use std::collections::HashMap;

/// Process-global cache of lazily-read method bodies, one [`EntryCache`] slot per classpath entry.
/// A body is keyed by the owning entry, so classpath-order shadowing stays per lookup while bodies
/// from an immutable library entry are shared by every classpath composition and compiler worker.
///
/// Only facts derived from one entry's bytes belong here. Composition-dependent records embed
/// shadowable classpath facts and must remain scoped to the complete [`super::Classpath`].
type BodyMap = HashMap<(TypeName, String, String), Option<MethodCode>>;
pub(super) type BodyCache = std::sync::Arc<std::sync::RwLock<BodyMap>>;

pub(super) fn global_entry_body_cache(key: &EntryKey) -> BodyCache {
    static CACHE: std::sync::OnceLock<EntryCache<std::sync::RwLock<BodyMap>>> =
        std::sync::OnceLock::new();
    CACHE
        .get_or_init(EntryCache::new)
        .get_or_build(key, Default::default)
}

/// Process-global cache of classes indexed for method-body decoding. A facade part commonly owns
/// hundreds of inline overloads over one large constant pool, so all of its bodies share one parsed
/// pool instead of repeatedly seeking, inflating, and decoding the same class. Failed reads are not
/// cached and can be retried after a transient filesystem error.
type ClassBodiesMap = HashMap<TypeName, std::sync::Arc<ClassBodies>>;
pub(super) type ClassBodiesCache = std::sync::Arc<std::sync::RwLock<ClassBodiesMap>>;

pub(super) fn global_entry_class_bodies_cache(key: &EntryKey) -> ClassBodiesCache {
    static CACHE: std::sync::OnceLock<EntryCache<std::sync::RwLock<ClassBodiesMap>>> =
        std::sync::OnceLock::new();
    CACHE
        .get_or_init(EntryCache::new)
        .get_or_build(key, Default::default)
}

impl Classpath {
    /// Read and index `internal_id` from one specific entry, without a classpath walk. Directory
    /// entries receive the same case-collision validation as the declaration lookup. `None` means
    /// the bytes could not be read; `Some(None)` means the class was read but its bodies are invalid.
    pub(super) fn entry_class_bodies(
        &self,
        entry_index: usize,
        internal_id: TypeName,
    ) -> Option<Option<std::sync::Arc<ClassBodies>>> {
        let cache = self.entry_class_bodies_caches.get(entry_index)?.as_ref();
        if let Some(hit) = cache.and_then(|cache| cache.read().unwrap().get(&internal_id).cloned())
        {
            return Some(Some(hit));
        }
        let internal = crate::jvm::names::classfile_internal_name_of(internal_id);
        let name = format!("{internal}.class");
        let bytes = match self.entries.get(entry_index)? {
            Entry::Dir(directory) => std::fs::read(directory.join(&name)).ok().filter(|bytes| {
                parse_class(bytes).is_ok_and(|class| class.this_class_matches(internal))
            }),
            Entry::Jar(jar) => self.jar_entry(jar, &name),
            Entry::Jimage(_) => self.jimage_bytes(internal),
            Entry::CtSym { path, release } => self.ct_sym_bytes(path, *release, internal),
        }?;
        let Some(class) = ClassBodies::parse(std::sync::Arc::new(bytes)) else {
            return Some(None);
        };
        let class = std::sync::Arc::new(class);
        if let Some(cache) = cache {
            cache.write().unwrap().insert(internal_id, class.clone());
        }
        Some(Some(class))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    use super::super::test_support::{test_temp_dir, write_test_jar_with_entry};

    // A metadata classifier can keep a dot in its final segment (`Outer.Inner`). The class file is
    // stored under `$`. The body read must use that physical spelling.
    #[test]
    fn method_body_of_a_dotted_classifier_reads_the_dollar_class_file() {
        let directory = test_temp_dir("dotted-body-class");
        std::fs::create_dir_all(&directory).expect("create temp dir");
        let mut cw = crate::jvm::classfile::ClassWriter::new(
            "probe/body6044/Outer$Inner",
            "java/lang/Object",
        );
        let mut code = crate::jvm::classfile::CodeBuilder::new(0);
        code.push_int(7, &mut cw);
        code.ireturn();
        cw.add_method(
            crate::jvm::classfile::ACC_PUBLIC | crate::jvm::classfile::ACC_STATIC,
            "answer",
            "()I",
            &code,
        );
        let jar = directory.join("nested.jar");
        write_test_jar_with_entry(&jar, "probe/body6044/Outer$Inner.class", &cw.finish());

        let dotted = crate::types::type_name_child(type_name("probe/body6044"), "Outer.Inner");
        let classpath = Classpath::new(vec![jar]);
        let code = classpath
            .method_code_name(dotted, "answer", "()I")
            .expect("dotted classifier reads Outer$Inner.class");
        assert_eq!(code.code.last().copied(), Some(0xac)); // ireturn

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove temp dir");
    }

    // `kotlin/Function1` is a metadata name. Its bytecode lives in `kotlin/jvm/functions/Function1`.
    #[test]
    fn method_body_of_a_function_classifier_reads_the_jvm_function_class() {
        let directory = test_temp_dir("function-body-class");
        std::fs::create_dir_all(&directory).expect("create temp dir");
        let mut cw = crate::jvm::classfile::ClassWriter::new(
            "kotlin/jvm/functions/Function1",
            "java/lang/Object",
        );
        let mut code = crate::jvm::classfile::CodeBuilder::new(0);
        code.push_int(3, &mut cw);
        code.ireturn();
        cw.add_method(
            crate::jvm::classfile::ACC_PUBLIC | crate::jvm::classfile::ACC_STATIC,
            "answer",
            "()I",
            &code,
        );
        let jar = directory.join("function.jar");
        write_test_jar_with_entry(&jar, "kotlin/jvm/functions/Function1.class", &cw.finish());

        let classpath = Classpath::new(vec![jar]);
        let code = classpath
            .method_code_name(type_name("kotlin/Function1"), "answer", "()I")
            .expect("Function1 reads kotlin/jvm/functions/Function1.class");
        assert_eq!(code.code.last().copied(), Some(0xac));

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove temp dir");
    }
}
