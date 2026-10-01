//! Entry-local caches for decoded JVM method bodies and their shared class indexes.

use super::{Classpath, Entry, EntryCache, EntryKey};
use crate::jvm::classreader::{parse_class, ClassBodies, MethodCode};
use crate::types::TypeName;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// A bounded method-body cache that owns physical member spellings for exactly as long as their
/// entries remain resident. Nested maps admit borrowed `&str` hits without allocating lookup keys,
/// and atomic recency permits shared entry-cache hits under a read lock.
pub(super) struct MethodBodyCache {
    cap: usize,
    tick: AtomicU64,
    len: usize,
    bodies: HashMap<TypeName, HashMap<Arc<str>, HashMap<Arc<str>, CachedBody>>>,
    recency: BinaryHeap<Reverse<BodyRecency>>,
}

struct CachedBody {
    body: Option<MethodCode>,
    tick: AtomicU64,
}

struct BodyRecency {
    owner: TypeName,
    name: Arc<str>,
    descriptor: Arc<str>,
    tick: u64,
}

impl PartialEq for BodyRecency {
    fn eq(&self, other: &Self) -> bool {
        self.tick == other.tick
    }
}

impl Eq for BodyRecency {}

impl PartialOrd for BodyRecency {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BodyRecency {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.tick.cmp(&other.tick)
    }
}

impl MethodBodyCache {
    pub(super) fn new(default_cap: usize) -> Self {
        let cap = std::env::var("KRUSTY_CACHE_CAP")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(default_cap);
        Self::with_cap(cap)
    }

    fn with_cap(cap: usize) -> Self {
        MethodBodyCache {
            cap: cap.max(1),
            tick: AtomicU64::new(0),
            len: 0,
            bodies: HashMap::new(),
            recency: BinaryHeap::new(),
        }
    }

    pub(super) fn get(
        &self,
        owner: TypeName,
        name: &str,
        descriptor: &str,
    ) -> Option<&Option<MethodCode>> {
        let tick = self.tick.fetch_add(1, Ordering::Relaxed) + 1;
        let cached = self.bodies.get(&owner)?.get(name)?.get(descriptor)?;
        cached.tick.fetch_max(tick, Ordering::Relaxed);
        Some(&cached.body)
    }

    #[cfg(test)]
    fn contains_key(&self, owner: TypeName, name: &str, descriptor: &str) -> bool {
        self.bodies
            .get(&owner)
            .and_then(|names| names.get(name))
            .is_some_and(|descriptors| descriptors.contains_key(descriptor))
    }

    pub(super) fn insert(
        &mut self,
        owner: TypeName,
        name: &str,
        descriptor: &str,
        body: Option<MethodCode>,
    ) {
        let tick = self.tick.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(cached) = self
            .bodies
            .get_mut(&owner)
            .and_then(|names| names.get_mut(name))
            .and_then(|descriptors| descriptors.get_mut(descriptor))
        {
            cached.body = body;
            cached.tick.store(tick, Ordering::Relaxed);
            return;
        }

        self.evict_if_full();
        let name: Arc<str> = Arc::from(name);
        let descriptor: Arc<str> = Arc::from(descriptor);
        self.bodies
            .entry(owner)
            .or_default()
            .entry(name.clone())
            .or_default()
            .insert(
                descriptor.clone(),
                CachedBody {
                    body,
                    tick: AtomicU64::new(tick),
                },
            );
        self.recency.push(Reverse(BodyRecency {
            owner,
            name,
            descriptor,
            tick,
        }));
        self.len += 1;
    }

    fn evict_if_full(&mut self) {
        if self.len < self.cap {
            return;
        }
        while let Some(Reverse(mut candidate)) = self.recency.pop() {
            let current = self
                .bodies
                .get(&candidate.owner)
                .and_then(|names| names.get(candidate.name.as_ref()))
                .and_then(|descriptors| descriptors.get(candidate.descriptor.as_ref()))
                .map(|cached| cached.tick.load(Ordering::Relaxed));
            match current {
                Some(tick) if tick == candidate.tick => {
                    self.remove(&candidate);
                    self.len -= 1;
                    return;
                }
                Some(tick) => {
                    candidate.tick = tick;
                    self.recency.push(Reverse(candidate));
                }
                None => {}
            }
        }
    }

    fn remove(&mut self, key: &BodyRecency) {
        let mut remove_owner = false;
        if let Some(names) = self.bodies.get_mut(&key.owner) {
            let mut remove_name = false;
            if let Some(descriptors) = names.get_mut(key.name.as_ref()) {
                descriptors.remove(key.descriptor.as_ref());
                remove_name = descriptors.is_empty();
            }
            if remove_name {
                names.remove(key.name.as_ref());
            }
            remove_owner = names.is_empty();
        }
        if remove_owner {
            self.bodies.remove(&key.owner);
        }
    }

    #[cfg(test)]
    fn key_text_owners(
        &self,
        owner: TypeName,
        name: &str,
        descriptor: &str,
    ) -> Option<(std::sync::Weak<str>, std::sync::Weak<str>)> {
        let names = self.bodies.get(&owner)?;
        let (name, descriptors) = names.get_key_value(name)?;
        let (descriptor, _) = descriptors.get_key_value(descriptor)?;
        Some((Arc::downgrade(name), Arc::downgrade(descriptor)))
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }
}

impl Default for MethodBodyCache {
    fn default() -> Self {
        Self::new(2048)
    }
}

pub(super) type BodyCache = std::sync::Arc<std::sync::RwLock<MethodBodyCache>>;

/// Process-global decoded bodies, one bounded [`EntryCache`] slot per immutable classpath entry.
/// Only facts derived from one entry's bytes belong here; composition-dependent facts stay scoped
/// to the complete [`Classpath`].
pub(super) fn global_entry_body_cache(key: &EntryKey) -> BodyCache {
    static CACHE: std::sync::OnceLock<EntryCache<std::sync::RwLock<MethodBodyCache>>> =
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
                parse_class(bytes).is_ok_and(|class| class.this_class_matches(&internal))
            }),
            Entry::Jar(jar) => self.jar_entry(jar, &name),
            Entry::Jimage(_) => self.jimage_bytes(&internal),
            Entry::CtSym { path, release } => self.ct_sym_bytes(path, *release, &internal),
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
    use crate::jvm::inline::MethodBodies;
    use crate::types::type_name;

    use super::super::test_support::{test_temp_dir, write_test_jar_with_entry};

    #[test]
    fn borrowed_hits_do_not_add_owned_method_spellings() {
        let owner = type_name("shared/Pool");
        let mut cache = MethodBodyCache::with_cap(2);
        cache.insert(owner, "answer", "()I", None);
        let owners = cache
            .key_text_owners(owner, "answer", "()I")
            .expect("owned cache key");

        for _ in 0..32 {
            let name = String::from("answer");
            let descriptor = String::from("()I");
            assert!(cache.get(owner, &name, &descriptor).is_some());
        }

        assert_eq!(cache.len(), 1);
        assert!(owners.0.upgrade().is_some());
        assert!(owners.1.upgrade().is_some());
    }

    #[test]
    fn cache_churn_releases_evicted_method_spellings() {
        let owner = type_name("shared/Pool");
        let mut cache = MethodBodyCache::with_cap(2);
        cache.insert(owner, "first", "()I", None);
        cache.insert(owner, "second", "()J", None);
        let evicted = cache
            .key_text_owners(owner, "second", "()J")
            .expect("second cache key");

        assert!(cache.get(owner, "first", "()I").is_some());
        cache.insert(owner, "third", "()V", None);

        assert_eq!(cache.len(), 2);
        assert!(cache.contains_key(owner, "first", "()I"));
        assert!(!cache.contains_key(owner, "second", "()J"));
        assert!(cache.contains_key(owner, "third", "()V"));
        assert!(evicted.0.upgrade().is_none());
        assert!(evicted.1.upgrade().is_none());
    }

    // A FAILED byte read must never populate the process-global body cache: a transient error
    // (EMFILE under load, an archive swapped mid-run) would otherwise be published as "no body"
    // for every compile sharing the entry key.
    #[test]
    fn failed_body_read_is_not_cached_process_globally() {
        let directory = test_temp_dir("transient-body-read");
        let jar = directory.join("lib.jar");
        write_test_jar_with_entry(&jar, "transient/Body.class", &body_class_bytes());

        let classpath = Classpath::new(vec![jar.clone()]);
        assert!(classpath.find("transient/Body").is_some());
        std::fs::remove_file(&jar).expect("delete jar between parse and body read");
        classpath.archives.borrow_mut().clear();
        assert!(classpath
            .method_code("transient/Body", "answer", "()I")
            .is_none());
        assert!(!global_entry_body_cache(&classpath.cache_key[0])
            .read()
            .unwrap()
            .contains_key(type_name("transient/Body"), "answer", "()I"));

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove temp dir");
    }

    // The body-cache key and bytes must come from the same parse-validated owning entry.
    #[test]
    fn body_bytes_come_from_the_owning_entry_not_the_first_raw_hit() {
        let directory = test_temp_dir("body-owner-attribution");
        let good = body_class_bytes();
        let mut corrupt = good.clone();
        corrupt[0] = 0;
        let earlier = directory.join("earlier.jar");
        let later = directory.join("later.jar");
        write_test_jar_with_entry(&earlier, "transient/Body.class", &corrupt);
        write_test_jar_with_entry(&later, "transient/Body.class", &good);

        let classpath = Classpath::new(vec![earlier, later]);
        assert!(classpath
            .method_code("transient/Body", "answer", "()I")
            .is_some());

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove temp dir");
    }

    #[test]
    fn archived_bodies_share_the_parsed_pool_and_identity_cache() {
        let directory = test_temp_dir("shared-body-pool");
        let mut writer = crate::jvm::classfile::ClassWriter::new("shared/Pool", "java/lang/Object");
        for (name, value) in [("first", 1), ("second", 2)] {
            let mut code = crate::jvm::classfile::CodeBuilder::new(0);
            code.push_int(value, &mut writer);
            code.ireturn();
            writer.add_method(
                crate::jvm::classfile::ACC_PUBLIC | crate::jvm::classfile::ACC_STATIC,
                name,
                "()I",
                &code,
            );
        }
        let jar = directory.join("pool.jar");
        write_test_jar_with_entry(&jar, "shared/Pool.class", &writer.finish());

        let classpath = Classpath::new(vec![jar]);
        let first = classpath
            .method_code("shared/Pool", "first", "()I")
            .expect("first body");
        let second = classpath
            .method_code_name(type_name("shared/Pool"), "second", "()I")
            .expect("second body");
        let string_second = classpath
            .method_code("shared/Pool", "second", "()I")
            .expect("string lookup agrees with the identity key");
        let repeated =
            MethodBodies::body_name(&classpath, type_name("shared/Pool"), "second", "()I")
                .expect("identity lookup reuses the body");

        assert!(Arc::ptr_eq(&first.source_cp, &second.source_cp));
        assert_ne!(first.code, second.code);
        assert_eq!(string_second.code, second.code);
        assert_eq!(repeated.code, second.code);
        assert!(global_entry_body_cache(&classpath.cache_key[0])
            .read()
            .unwrap()
            .contains_key(type_name("shared/Pool"), "second", "()I"));

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove temp dir");
    }

    // A metadata classifier can keep a dot in its final segment (`Outer.Inner`). The class file is
    // stored under `$`. The body read must use that physical spelling.
    #[test]
    fn method_body_of_a_dotted_classifier_reads_the_dollar_class_file() {
        let directory = test_temp_dir("dotted-body-class");
        let mut writer = crate::jvm::classfile::ClassWriter::new(
            "probe/body6044/Outer$Inner",
            "java/lang/Object",
        );
        let mut code = crate::jvm::classfile::CodeBuilder::new(0);
        code.push_int(7, &mut writer);
        code.ireturn();
        writer.add_method(
            crate::jvm::classfile::ACC_PUBLIC | crate::jvm::classfile::ACC_STATIC,
            "answer",
            "()I",
            &code,
        );
        let jar = directory.join("nested.jar");
        write_test_jar_with_entry(&jar, "probe/body6044/Outer$Inner.class", &writer.finish());

        let dotted = crate::types::type_name_child(type_name("probe/body6044"), "Outer.Inner");
        let classpath = Classpath::new(vec![jar]);
        let code = classpath
            .method_code_name(dotted, "answer", "()I")
            .expect("dotted classifier reads Outer$Inner.class");
        assert_eq!(code.code.last().copied(), Some(0xac));

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove temp dir");
    }

    // `kotlin/Function1` is a metadata name. Its bytecode lives in `kotlin/jvm/functions/Function1`.
    #[test]
    fn method_body_of_a_function_classifier_reads_the_jvm_function_class() {
        let directory = test_temp_dir("function-body-class");
        let mut writer = crate::jvm::classfile::ClassWriter::new(
            "kotlin/jvm/functions/Function1",
            "java/lang/Object",
        );
        let mut code = crate::jvm::classfile::CodeBuilder::new(0);
        code.push_int(3, &mut writer);
        code.ireturn();
        writer.add_method(
            crate::jvm::classfile::ACC_PUBLIC | crate::jvm::classfile::ACC_STATIC,
            "answer",
            "()I",
            &code,
        );
        let jar = directory.join("function.jar");
        write_test_jar_with_entry(
            &jar,
            "kotlin/jvm/functions/Function1.class",
            &writer.finish(),
        );

        let classpath = Classpath::new(vec![jar]);
        let code = classpath
            .method_code_name(type_name("kotlin/Function1"), "answer", "()I")
            .expect("Function1 reads kotlin/jvm/functions/Function1.class");
        assert_eq!(code.code.last().copied(), Some(0xac));

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove temp dir");
    }

    fn body_class_bytes() -> Vec<u8> {
        let mut writer =
            crate::jvm::classfile::ClassWriter::new("transient/Body", "java/lang/Object");
        let mut code = crate::jvm::classfile::CodeBuilder::new(0);
        code.push_int(1, &mut writer);
        code.ireturn();
        writer.add_method(
            crate::jvm::classfile::ACC_PUBLIC | crate::jvm::classfile::ACC_STATIC,
            "answer",
            "()I",
            &code,
        );
        writer.finish()
    }
}
