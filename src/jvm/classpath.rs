//! Classpath: resolve an internal class name (e.g. `util/Calc`) to its `ClassInfo` from either a
//! directory of loose `.class` files **or a `.jar`** (Java/Kotlin library support). Results are
//! cached. jar entries are read on demand (DEFLATE via the `zip` crate).
//!
//! Extension function index: scans all classpath classes for static methods whose first parameter
//! matches a given descriptor. Used to resolve Kotlin extension functions (e.g. `str.uppercase()`)
//! from any library JAR without hardcoding method lists.
//!
//! Type index: scans all classpath classes to build:
//! - `simple_name → internal_name` for every class in the classpath
//! - Kotlin type aliases from `@kotlin.Metadata` `d2` arrays in `*TypeAliasesKt.class` files

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::jvm::classreader::{parse_class, read_method_code, ClassInfo, MethodCode};
use crate::jvm::names::type_descriptor;
use crate::libraries::{CallSig, ReturnInfo};
use crate::name_tree::{NameId, NameTree};
use crate::types::{type_name, type_name_from, Ty, TypeName, TypeNameList};

/// Map a Kotlin internal type name (`kotlin/Int`, `kotlin/Char`, …) from builtins metadata to a `Ty`.
pub(super) fn kotlin_name_to_ty(name: &str) -> Ty {
    match name {
        "kotlin/Int" => Ty::Int,
        "kotlin/Char" => Ty::Char,
        "kotlin/Boolean" => Ty::Boolean,
        "kotlin/Long" => Ty::Long,
        "kotlin/Double" => Ty::Double,
        "kotlin/Float" => Ty::Float,
        "kotlin/Byte" => Ty::Byte,
        "kotlin/Short" => Ty::Short,
        "kotlin/UInt" => Ty::UInt,
        "kotlin/ULong" => Ty::ULong,
        "kotlin/String" => Ty::String,
        "kotlin/Unit" => Ty::Unit,
        "kotlin/Nothing" => Ty::Nothing,
        _ => Ty::obj(name),
    }
}

/// Id-backed form of [`kotlin_name_to_ty`].
pub(super) fn kotlin_type_name_to_ty(name: TypeName) -> Ty {
    if name.matches("kotlin/Int") {
        Ty::Int
    } else if name.matches("kotlin/Char") {
        Ty::Char
    } else if name.matches("kotlin/Boolean") {
        Ty::Boolean
    } else if name.matches("kotlin/Long") {
        Ty::Long
    } else if name.matches("kotlin/Double") {
        Ty::Double
    } else if name.matches("kotlin/Float") {
        Ty::Float
    } else if name.matches("kotlin/Byte") {
        Ty::Byte
    } else if name.matches("kotlin/Short") {
        Ty::Short
    } else if name.matches("kotlin/UInt") {
        Ty::UInt
    } else if name.matches("kotlin/ULong") {
        Ty::ULong
    } else if name.matches("kotlin/String") {
        Ty::String
    } else if name.matches("kotlin/Unit") {
        Ty::Unit
    } else if name.matches("kotlin/Nothing") {
        Ty::Nothing
    } else {
        Ty::obj_name(name)
    }
}

fn meta_function_arity_name(name: TypeName) -> Option<usize> {
    name.unsigned_suffix_after_prefix("kotlin/Function")
}

fn primitive_array_descriptor_name(internal: TypeName) -> Option<&'static str> {
    Some(if internal.matches("kotlin/BooleanArray") {
        "[Z"
    } else if internal.matches("kotlin/ByteArray") {
        "[B"
    } else if internal.matches("kotlin/ShortArray") {
        "[S"
    } else if internal.matches("kotlin/IntArray") {
        "[I"
    } else if internal.matches("kotlin/LongArray") || internal.matches("kotlin/ULongArray") {
        "[J"
    } else if internal.matches("kotlin/CharArray") {
        "[C"
    } else if internal.matches("kotlin/FloatArray") {
        "[F"
    } else if internal.matches("kotlin/DoubleArray") {
        "[D"
    } else if internal.matches("kotlin/UIntArray") {
        "[I"
    } else {
        return None;
    })
}

fn ty_erases_to_object(desc: Ty) -> bool {
    matches!(desc, Ty::Obj(n, _) if n.matches("kotlin/Any") || n.matches("java/lang/Object"))
}

/// Whether a `@Metadata` source value-parameter class name aligns with a JVM-descriptor parameter `Ty`.
/// This keeps the hot overload-alignment path in borrowed names: mapped builtins compare through
/// `to_jvm_internal`, arrays/functions use structural `Ty` facts, and no descriptor `String` is built just
/// to decide whether two class names denote the same erased JVM parameter.
fn meta_param_compat(name: Option<TypeName>, desc: &Ty) -> bool {
    let Some(name) = name else {
        return desc.is_reference();
    };
    if let Some(arity) = meta_function_arity_name(name) {
        return matches!(desc, Ty::Fun(sig) if sig.params.len() == arity);
    }
    if name.matches("kotlin/Array") || primitive_array_descriptor_name(name).is_some() {
        return desc.is_array();
    }
    if name.matches("kotlin/Int") {
        *desc == Ty::Int
    } else if name.matches("kotlin/Char") {
        *desc == Ty::Char
    } else if name.matches("kotlin/Boolean") {
        *desc == Ty::Boolean
    } else if name.matches("kotlin/Long") {
        *desc == Ty::Long
    } else if name.matches("kotlin/Double") {
        *desc == Ty::Double
    } else if name.matches("kotlin/Float") {
        *desc == Ty::Float
    } else if name.matches("kotlin/Byte") {
        *desc == Ty::Byte
    } else if name.matches("kotlin/Short") {
        *desc == Ty::Short
    } else if name.matches("kotlin/UInt") {
        matches!(*desc, Ty::UInt | Ty::Int)
    } else if name.matches("kotlin/ULong") {
        matches!(*desc, Ty::ULong | Ty::Long)
    } else if name.matches("kotlin/Unit") {
        *desc == Ty::Unit
    } else if name.matches("kotlin/Nothing") {
        *desc == Ty::Nothing
    } else if name.matches("kotlin/Any") && desc.is_reference() {
        true
    } else if matches!(*desc, Ty::String) {
        name.matches("kotlin/String") || name.matches("java/lang/String")
    } else if desc.obj_internal().is_some_and(|desc_internal| {
        crate::jvm::jvm_class_map::type_names_map_to_same_jvm_internal(desc_internal, name)
    }) {
        true
    } else {
        ty_erases_to_object(*desc) && !desc.is_array()
    }
}

fn meta_param_exact(name: Option<TypeName>, desc: &Ty) -> bool {
    let Some(name) = name else {
        return ty_erases_to_object(*desc);
    };
    if let Some(arity) = meta_function_arity_name(name) {
        return matches!(desc, Ty::Fun(sig) if sig.params.len() == arity);
    }
    if name.matches("kotlin/Array") {
        return matches!(desc, Ty::Obj(n, args)
            if n.matches("kotlin/Array") && args.first().copied().is_some_and(ty_erases_to_object));
    }
    if let Some(meta_desc) = primitive_array_descriptor_name(name) {
        return desc
            .obj_internal()
            .and_then(primitive_array_descriptor_name)
            == Some(meta_desc);
    }
    if name.matches("kotlin/Int") {
        *desc == Ty::Int
    } else if name.matches("kotlin/Char") {
        *desc == Ty::Char
    } else if name.matches("kotlin/Boolean") {
        *desc == Ty::Boolean
    } else if name.matches("kotlin/Long") {
        *desc == Ty::Long
    } else if name.matches("kotlin/Double") {
        *desc == Ty::Double
    } else if name.matches("kotlin/Float") {
        *desc == Ty::Float
    } else if name.matches("kotlin/Byte") {
        *desc == Ty::Byte
    } else if name.matches("kotlin/Short") {
        *desc == Ty::Short
    } else if name.matches("kotlin/UInt") {
        matches!(*desc, Ty::UInt | Ty::Int)
    } else if name.matches("kotlin/ULong") {
        matches!(*desc, Ty::ULong | Ty::Long)
    } else if name.matches("kotlin/Unit") {
        *desc == Ty::Unit
    } else if name.matches("kotlin/Nothing") {
        *desc == Ty::Nothing
    } else if matches!(*desc, Ty::String) {
        name.matches("kotlin/String") || name.matches("java/lang/String")
    } else {
        desc.obj_internal().is_some_and(|desc_internal| {
            crate::jvm::jvm_class_map::type_names_map_to_same_jvm_internal(desc_internal, name)
        })
    }
}

enum Entry {
    Dir(PathBuf),
    Jar(PathBuf),
    /// A JDK `lib/modules` jimage container (the JVM bootclasspath). Added explicitly to the
    /// classpath, exactly like a jar — there is no implicit `JAVA_HOME` lookup.
    Jimage(PathBuf),
}

impl Entry {
    fn path(&self) -> &Path {
        match self {
            Entry::Dir(p) | Entry::Jar(p) | Entry::Jimage(p) => p,
        }
    }
}

/// Process-global `scan_types` results keyed by the entry path set. The JDK jimage and stdlib jars
/// are identical across every compiled file, so this collapses N re-scans into one.
fn global_type_cache() -> &'static std::sync::Mutex<HashMap<Vec<PathBuf>, std::sync::Arc<TypeIndex>>>
{
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<Vec<PathBuf>, std::sync::Arc<TypeIndex>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Record a classpath-cache hit (`true`) or miss (`false`) for the named counter. Compiled out ENTIRELY
/// unless built `--features trace` — so normal/release builds pay nothing (no atomic, no cache-line
/// contention on the hot lookup paths). Under the feature, view the summary with `KRUSTY_TRACE=cache`.
macro_rules! cache_stat {
    ($field:ident, $hit:expr) => {{
        #[cfg(feature = "trace")]
        {
            cache_stats().$field.record($hit);
        }
        #[cfg(not(feature = "trace"))]
        {
            let _ = $hit;
        }
    }};
}

/// Hit/miss counter for one cache, aggregated across every `Classpath` and worker thread (per-instance
/// caches are short-lived, so only a process-global tally shows whole-run efficiency).
#[cfg(feature = "trace")]
#[derive(Default)]
struct CacheCounter {
    hit: std::sync::atomic::AtomicU64,
    miss: std::sync::atomic::AtomicU64,
}

#[cfg(feature = "trace")]
impl CacheCounter {
    #[inline]
    fn record(&self, hit: bool) {
        let c = if hit { &self.hit } else { &self.miss };
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn line(&self, name: &str) -> String {
        let h = self.hit.load(std::sync::atomic::Ordering::Relaxed);
        let m = self.miss.load(std::sync::atomic::Ordering::Relaxed);
        let t = h + m;
        let rate = if t == 0 {
            0.0
        } else {
            100.0 * h as f64 / t as f64
        };
        format!("{name} {h}/{t} ({rate:.1}%)")
    }
}

/// Process-wide cache hit/miss tallies. Each field tracks one cache; a MISS on a level means the lookup
/// fell through to the next level (L1_class miss → try L2; L2_class miss → parse from disk). Compare
/// L1 vs L2 hit rates to see whether the per-thread cap is too small, and the fall-through (miss) counts
/// to see how often a level actually saves work.
#[cfg(feature = "trace")]
#[derive(Default)]
struct CacheStats {
    l1_class: CacheCounter,
    l2_class: CacheCounter,
    ext_l1: CacheCounter,
    ext_l2: CacheCounter,
    meta_fns: CacheCounter,
    bodies: CacheCounter,
    builtin_members: CacheCounter,
}

#[cfg(feature = "trace")]
fn cache_stats() -> &'static CacheStats {
    static S: std::sync::OnceLock<CacheStats> = std::sync::OnceLock::new();
    S.get_or_init(CacheStats::default)
}

/// Emit the whole-process cache hit-rate summary through the `cache` trace category — a single line,
/// only when built `--features trace` and `KRUSTY_TRACE=cache` (or `all`). No-op otherwise, so callers
/// (e.g. the box harness at end of a run) can invoke it unconditionally.
pub fn trace_cache_stats() {
    #[cfg(feature = "trace")]
    {
        let s = cache_stats();
        crate::trace_compiler!(
            "cache",
            "class L1 {} · L2 {} | ext L1 {} · L2 {} | meta_fns {} | bodies {} | builtin {}",
            s.l1_class.line("hits"),
            s.l2_class.line("hits"),
            s.ext_l1.line("hits"),
            s.ext_l2.line("hits"),
            s.meta_fns.line("hits"),
            s.bodies.line("hits"),
            s.builtin_members.line("hits"),
        );
    }
}

/// One jimage resource: `(file offset, ON-DISK byte size, zlib-compressed?)`. The size is the stored
/// (compressed) length when the resource uses the "zip" decompressor, else the raw class length; the
/// flag is set ONLY for the "zip" decompressor (authoritatively, from the strings table) so the reader
/// never inflates a resource compressed by some other scheme.
type JimageEntry = (u64, usize, bool);

#[derive(Default, Debug)]
struct JimageIndex {
    names: NameTree,
    by_name: HashMap<NameId, JimageEntry>,
}

/// Process-global jimage index (name id → file offset/size), keyed by the jimage path. The jimage is
/// identical for every compiled file, so parsing its 146 MB happens once per process, not per thread.
fn global_jimage_cache() -> &'static std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<JimageIndex>>>
{
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<JimageIndex>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Process-global extension/top-level-function index, keyed by the classpath path set. Scanning every
/// jar's static methods is identical for a given classpath, so it happens once per process rather than
/// once per worker thread (the box harness compiles thousands of files across all cores against the
/// same stdlib classpath) — the same sharing as [`global_type_cache`]/[`global_jimage_cache`].
fn global_ext_cache() -> &'static std::sync::Mutex<HashMap<Vec<PathBuf>, std::sync::Arc<ExtIndex>>>
{
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<Vec<PathBuf>, std::sync::Arc<ExtIndex>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// A process-global cache of a value derived from a SINGLE classpath entry (jar / dir / jimage), keyed
/// by that entry's path. A jar's classes, extension statics, and type aliases are identical wherever the
/// jar appears, so its contribution is built ONCE and shared by every classpath that includes it — a
/// classpath that only adds one library reuses every other entry's cached contribution and builds just
/// the new one. This is the composable layer UNDER the whole-classpath indexes: compose an index per cp
/// from these per-entry parts instead of rescanning every jar when the cp differs by a single entry.
struct EntryCache<T> {
    map: std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<T>>>,
}

impl<T> EntryCache<T> {
    fn new() -> Self {
        EntryCache {
            map: std::sync::Mutex::new(HashMap::new()),
        }
    }
    /// The entry's cached value, built once via `build` on first request. The map lock is held across
    /// the build so worker threads starting together build each entry exactly once, not N times (this
    /// subsumes the ad-hoc per-index build locks).
    fn get_or_build(&self, path: &Path, build: impl FnOnce() -> T) -> std::sync::Arc<T> {
        let mut map = self.map.lock().unwrap();
        if let Some(v) = map.get(path) {
            return v.clone();
        }
        let v = std::sync::Arc::new(build());
        map.insert(path.to_path_buf(), v.clone());
        v
    }
}

fn push_id_dedup(m: &mut HashMap<String, Vec<NameId>>, key: &str, id: NameId) {
    let v = m.entry(key.to_string()).or_default();
    if v.last().copied() != Some(id) && !v.contains(&id) {
        v.push(id);
    }
}

fn push_name_from_dedup(
    names: &mut NameTree,
    m: &mut HashMap<String, Vec<NameId>>,
    key: &str,
    source_names: &NameTree,
    source_id: NameId,
) {
    let id = names.insert_from(source_names, source_id);
    push_id_dedup(m, key, id);
}

/// Per-ENTRY extension-index contributions (one per jar/dir), composed per classpath by
/// [`Classpath::ensure_ext_index`]. See [`EntryCache`].
fn global_entry_ext() -> &'static EntryCache<EntryExt> {
    static CACHE: std::sync::OnceLock<EntryCache<EntryExt>> = std::sync::OnceLock::new();
    CACHE.get_or_init(EntryCache::new)
}

/// Per-ENTRY package catalogs (one [`JarPackages`] per jar/dir), composed into the per-classpath
/// [`PackageTree`] by [`Classpath::package_tree`]. See [`EntryCache`].
fn global_jar_packages() -> &'static EntryCache<JarPackages> {
    static CACHE: std::sync::OnceLock<EntryCache<JarPackages>> = std::sync::OnceLock::new();
    CACHE.get_or_init(EntryCache::new)
}

/// Per-ENTRY type-alias tables (one [`TypeIndex`] per jar/dir), composed per classpath by
/// [`Classpath::scan_types`]. See [`EntryCache`] — the build holds the map lock, so each jar's
/// "parse every `*Kt` facade for aliases" scan runs ONCE for the whole process instead of racing
/// across every worker thread on cold start (the cost the box-conformance flamegraph flagged).
fn global_entry_types() -> &'static EntryCache<TypeIndex> {
    static CACHE: std::sync::OnceLock<EntryCache<TypeIndex>> = std::sync::OnceLock::new();
    CACHE.get_or_init(EntryCache::new)
}

/// The spec's `(jar, package) → PkgMembers`: a per-(jar, package) index of the package's static
/// callables, parsed once from that jar's `kotlin_module` facades and SHARED across every classpath that
/// includes the jar (keyed by jar path + package), exactly like the other per-entry caches. Three
/// indices from ONE facade-statics pass so every scoped query is O(1): [`Self::by_source`] for a
/// source-name lookup (top-level/extension resolution), [`Self::by_jvm`] for a JVM-name lookup (the
/// mangled `@JvmName` extension paths), and [`Self::owners_by_recv`] for the receiver-descriptor →
/// declaring-facade query. `Arc` so a package touched by many worker threads is parsed once.
#[derive(Default)]
struct PkgMembers {
    owner_names: NameTree,
    candidates: Vec<ExtCandidateRecord>,
    /// Static callables keyed by their `@Metadata` SOURCE name (`sum`), for the source-name resolution.
    by_source: HashMap<String, Vec<usize>>,
    /// The same callables keyed by their JVM method name (`sumOfInt`), for the literal-name extension
    /// lookup that mirrors [`Classpath::find_extensions`] (which keys by the bytecode name).
    by_jvm: HashMap<String, Vec<usize>>,
    /// Receiver (first-parameter) descriptor → the facades declaring a static with that receiver — the
    /// scoped analogue of [`Classpath::find_extension_owners`]. Deduped, declaration order.
    owners_by_recv: HashMap<String, Vec<NameId>>,
}

impl PkgMembers {
    fn render_indices(&self, indices: &[usize]) -> Vec<ExtCandidate> {
        indices
            .iter()
            .filter_map(|&i| self.candidates.get(i))
            .map(|c| c.render(&self.owner_names))
            .collect()
    }
}

type JarPkgMembers = std::sync::Arc<PkgMembers>;
fn global_jar_pkg_members() -> &'static std::sync::Mutex<HashMap<(PathBuf, String), JarPkgMembers>>
{
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<(PathBuf, String), JarPkgMembers>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Process-global composed package table, keyed by the classpath entry set — like [`global_type_cache`],
/// the stdlib/JDK entries are identical across every compiled file, so the compose runs once per process.
fn global_pkg_tree_cache(
) -> &'static std::sync::Mutex<HashMap<Vec<PathBuf>, std::sync::Arc<PackageTree>>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<Vec<PathBuf>, std::sync::Arc<PackageTree>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// The rebuilt candidates for ONE method name, grouped for O(1) receiver lookup so `find_extensions`
/// doesn't re-scan + re-parse the whole list on every call site (the cost the eager `by_recv` map avoided).
#[derive(Default)]
struct ExtByName {
    owner_names: NameTree,
    /// first-parameter descriptor (the extension receiver) → indices into [`Self::all`].
    by_recv: HashMap<String, Vec<usize>>,
    /// every candidate of this name (top-level + extensions), for the receiver-less `find_top_level`.
    all: Vec<ExtCandidateRecord>,
}

impl ExtByName {
    fn render_by_recv(&self, receiver_desc: &str) -> Vec<ExtCandidate> {
        self.by_recv
            .get(receiver_desc)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&i| self.all.get(i))
                    .map(|c| c.render(&self.owner_names))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn render_all(&self) -> Vec<ExtCandidate> {
        self.render_candidates(&self.all)
    }

    fn render_candidates(&self, cands: &[ExtCandidateRecord]) -> Vec<ExtCandidate> {
        cands.iter().map(|c| c.render(&self.owner_names)).collect()
    }
}

#[derive(Clone, Debug)]
struct ExtCandidateRecord {
    owner: NameId,
    name: String,
    descriptor: String,
    ret_desc: String,
    signature: Option<String>,
    public: bool,
}

impl ExtCandidateRecord {
    fn from_candidate(owner: NameId, cand: &ExtCandidate) -> Self {
        ExtCandidateRecord {
            owner,
            name: cand.name.clone(),
            descriptor: cand.descriptor.clone(),
            ret_desc: cand.ret_desc.clone(),
            signature: cand.signature.clone(),
            public: cand.public,
        }
    }

    fn render(&self, owner_names: &NameTree) -> ExtCandidate {
        ExtCandidate {
            owner: type_name_from(owner_names, self.owner),
            name: self.name.clone(),
            descriptor: self.descriptor.clone(),
            ret_desc: self.ret_desc.clone(),
            signature: self.signature.clone(),
            public: self.public,
        }
    }
}

/// Process-global memoization of the lazy ext index's REBUILT candidates (method name → grouped
/// candidates), keyed by classpath and SHARED across worker threads. The rebuild (super-walk of a name's
/// facades) then runs once per name for the whole process, not once per thread; grouping by receiver keeps
/// the per-call-site `find_extensions` O(1). `RwLock` because hits (reads) dominate; a miss takes the write
/// lock briefly. Bounded by the DISTINCT QUERIED names (the working set), not the whole classpath.
type ExtCandCache = std::sync::Arc<std::sync::RwLock<HashMap<String, std::sync::Arc<ExtByName>>>>;
fn global_ext_candidates(key: &[PathBuf]) -> ExtCandCache {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<Vec<PathBuf>, ExtCandCache>>> =
        std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .entry(key.to_vec())
        .or_insert_with(|| std::sync::Arc::new(std::sync::RwLock::new(HashMap::new())))
        .clone()
}

/// Process-global cache of parsed `ClassInfo` (internal-name id → parsed class, `None` if absent), keyed
/// by the classpath. The conformance harness compiles on several rayon worker threads, EACH with its
/// own `Classpath`; without sharing, every common class (`kotlin/collections/List`, …) was parsed once
/// per thread. Sharing this — like the type/ext/jimage indexes — parses each class once per process.
/// `RwLock` because reads (cache hits) dominate; a parse on a miss takes the write lock briefly.
struct ClassCacheData {
    classes: std::sync::RwLock<HashMap<TypeName, Option<std::sync::Arc<ClassInfo>>>>,
}

impl Default for ClassCacheData {
    fn default() -> Self {
        ClassCacheData {
            classes: std::sync::RwLock::new(HashMap::new()),
        }
    }
}

impl ClassCacheData {
    fn len(&self) -> usize {
        self.classes.read().unwrap().len()
    }
}

type ClassCache = std::sync::Arc<ClassCacheData>;
fn global_class_cache(key: &[PathBuf]) -> ClassCache {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<Vec<PathBuf>, ClassCache>>> =
        std::sync::OnceLock::new();
    let m = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut g = m.lock().unwrap();
    g.entry(key.to_vec())
        .or_insert_with(|| std::sync::Arc::new(ClassCacheData::default()))
        .clone()
}

/// One resolved extension-function candidate: the owner class (internal name), the JVM method
/// descriptor, the method name, and the return-type descriptor.
#[derive(Clone, Debug)]
pub struct ExtCandidate {
    pub owner: TypeName,
    pub name: String,
    pub descriptor: String,
    pub ret_desc: String,
    /// The method's generic `Signature` attribute, if any — for recovering the parameterized return
    /// type of a generic top-level function (`listOf<T>` → `List<T>`).
    pub signature: Option<String>,
    /// `true` for a public method. A non-public static (an `@InlineOnly` stdlib scope fn) is indexed so
    /// the bytecode inliner can splice it, but the resolver admits it only for inline-only selection,
    /// never as a callable (an `invokestatic` to a package-private method would `IllegalAccessError`).
    pub public: bool,
}

/// Lazy index of static methods grouped by `(first_param_descriptor, method_name)`. Built on
/// first use from all entries in the classpath.
/// A LAZY index of the classpath's static (top-level + extension) functions. Only the small "where" map
/// is retained — `name → the facade/part ROOT classes that declare a static of that name`; the full
/// candidate records (descriptors, signatures) are REBUILT on query from each root's `ClassInfo` (which
/// the L1/L2 caches already hold), via [`Classpath::rebuild_ext_candidate_records`]. This keeps ~a few MB
/// of name/owner strings resident instead of materializing every stdlib static twice (the old eager index
/// was the single largest retained allocation — ~195 MB — per heap profiling).
#[derive(Default)]
struct ExtIndex {
    owner_names: NameTree,
    /// method name → facade/part ROOT class names whose super-walk declares a static of that name.
    by_name: HashMap<String, Vec<NameId>>,
    /// receiver descriptor → the owner facades that declare an extension on it (for `find_extension_owners`).
    by_recv_owners: HashMap<String, Vec<NameId>>,
    /// Names `@Metadata` marks as GENUINE top-level (a receiver-less function, never an extension) — these
    /// are never keyed by their first parameter, so `find_extensions` must not return them for any receiver.
    toplevel_only: std::collections::HashSet<String>,
}

/// ONE classpath entry's contribution to the extension index, built once per jar/dir and composed per
/// classpath (see [`EntryCache`] / [`Classpath::ensure_ext_index`]). `by_recv_raw` stays UNFILTERED — the
/// `toplevel_only` decision is global across the whole cp, so it can only be applied when composing.
#[derive(Default)]
struct EntryExt {
    owner_names: NameTree,
    /// method name → owner ROOT classes in THIS entry (super-walk within the entry).
    by_name: HashMap<String, Vec<NameId>>,
    /// receiver descriptor → `(method name, owner)` for each receiver-taking static in this entry.
    by_recv_raw: HashMap<String, Vec<(String, NameId)>>,
    /// JVM names this entry marks as genuine top-level, and as extensions (unioned across the cp at
    /// compose to decide `toplevel_only = union(top) - union(ext)`).
    toplevel_names: std::collections::HashSet<String>,
    ext_names: std::collections::HashSet<String>,
}

/// Classpath Kotlin type aliases (`typealias X = Y` in a library), simple alias name → target-name ID.
/// A simple/FQ name → internal CLASS map used to live here too, but name resolution is import-driven (via
/// `resolve_type` probes and the ext index's `resolve_top_level_callable`), not table-driven — verified by
/// building it empty with no test regression. Building it eagerly for every class on the classpath (the
/// whole ~30k-class JDK jimage included) was ~85 MB of retained dead weight + a full-image name scan.
#[derive(Default, Clone, Debug)]
pub struct TypeIndex {
    /// Kotlin type alias name → target JVM internal name
    /// (e.g. `"StringBuilder"` → `"java/lang/StringBuilder"`).
    type_aliases: HashMap<TypeName, TypeName>,
}

impl TypeIndex {
    pub fn is_empty(&self) -> bool {
        self.type_aliases.is_empty()
    }
}

/// Per-class `@Metadata` cache: class internal name → every function decoded from its `Package` metadata
/// (with the multifile-facade part classes merged in). This is the SINGLE decode of a class's `d1` for the
/// function lookups below — `meta_functions`, `metadata_call_facts`, and parameter metadata all project
/// over it instead of each re-decoding and re-merging.
type MetaFnsCache = RefCell<crate::lru::LruCache<TypeName, std::rc::Rc<ClassMeta>>>;

#[derive(Clone)]
pub struct MetadataCallFacts {
    pub kept_params: Option<usize>,
    pub call_sig: CallSig,
    pub ret: ReturnInfo,
}

impl MetadataCallFacts {
    fn fallback(call_sig: CallSig) -> Self {
        MetadataCallFacts {
            kept_params: None,
            call_sig,
            ret: ReturnInfo::default(),
        }
    }
}

/// The per-function `@Metadata` lookups for one class, all derived from its single decoded function list
/// (facade parts merged). Computed once per class in [`Classpath::class_meta`].
struct ClassMeta {
    by_jvm_name: HashMap<String, Vec<usize>>,
    suspend_names: HashSet<String>,
    /// The full facade-merged [`MetaFn`] list this is projected from — exposed via
    /// [`Classpath::meta_functions`] for the lookups that need a whole `MetaFn` (return class by JVM
    /// name, receiver-function params) rather than one of the maps above, so they share THIS decode
    /// instead of re-decoding + re-merging the `d1` themselves.
    fns: std::rc::Rc<[super::metadata::MetaFn]>,
}

#[derive(Default)]
struct BuiltinsFile {
    classes: HashMap<TypeName, BuiltinClass>,
}

struct BuiltinClass {
    supertypes: TypeNameList,
    members: Vec<BuiltinMember>,
    is_interface: bool,
    nullable_member_returns: Vec<(String, usize)>,
}

struct BuiltinMember {
    name: String,
    params: Vec<BuiltinType>,
    ret: BuiltinType,
    is_property: bool,
    ret_nullable: bool,
}

enum BuiltinType {
    Class(TypeName),
    Param(String),
}

impl BuiltinType {
    fn from_metadata(name: String) -> Self {
        if name.contains('/') {
            BuiltinType::Class(type_name(&name))
        } else {
            BuiltinType::Param(name)
        }
    }

    fn descriptor(&self) -> String {
        match self {
            BuiltinType::Class(name) => type_descriptor(kotlin_type_name_to_ty(*name)),
            BuiltinType::Param(_) => "Ljava/lang/Object;".to_string(),
        }
    }

    fn ty(&self) -> Ty {
        match self {
            BuiltinType::Class(name) => kotlin_type_name_to_ty(*name),
            BuiltinType::Param(name) => Ty::obj(name),
        }
    }

    fn is_class(&self) -> bool {
        matches!(self, BuiltinType::Class(_))
    }
}

impl BuiltinsFile {
    fn from_classes(classes: HashMap<String, super::metadata::BuiltinClass>) -> Self {
        let mut file = BuiltinsFile::default();
        for (internal, class) in classes {
            let internal = type_name(&internal);
            let supertypes = class
                .supertypes
                .into_iter()
                .map(|name| type_name(&name))
                .collect::<Vec<_>>()
                .into();
            let members = class
                .members
                .into_iter()
                .map(|m| BuiltinMember {
                    name: m.name,
                    params: m
                        .params
                        .into_iter()
                        .map(BuiltinType::from_metadata)
                        .collect(),
                    ret: BuiltinType::from_metadata(m.ret),
                    is_property: m.is_property,
                    ret_nullable: m.ret_nullable,
                })
                .collect();
            file.classes.insert(
                internal,
                BuiltinClass {
                    supertypes,
                    members,
                    is_interface: class.is_interface,
                    nullable_member_returns: class.nullable_member_returns,
                },
            );
        }
        file
    }

    fn get(&self, internal: &str) -> Option<&BuiltinClass> {
        self.classes.get(&type_name(internal))
    }

    fn get_name(&self, internal: TypeName) -> Option<&BuiltinClass> {
        self.classes.get(&internal)
    }

    fn contains_key(&self, internal: &str) -> bool {
        self.get(internal).is_some()
    }

    fn contains_key_name(&self, internal: TypeName) -> bool {
        self.get_name(internal).is_some()
    }

    fn is_subtype(&self, sub: &str, sup: &str) -> bool {
        self.is_subtype_name(type_name(sub), type_name(sup))
    }

    fn is_subtype_name(&self, sub: TypeName, sup: TypeName) -> bool {
        sub == sup
            || self.classes.get(&sub).is_some_and(|c| {
                c.supertypes
                    .iter_ids()
                    .any(|s| self.is_subtype_name(s, sup))
            })
    }
}

/// Whether metadata callable `c` corresponds to a JVM method with these descriptor parameter types. An
/// EXTENSION's receiver — a separate attribute, emitted as the leading JVM parameter — must match, then
/// the value parameters align in order. Returns `(kept-param end, exact-match count)` — `end` is the count
/// of SOURCE parameters (where the synthetic tail — a `suspend` Continuation, a `$default` mask — begins),
/// and `exact` counts the value params matching by EQUAL erased descriptor (not through the loose
/// type-variable rule), so the caller prefers the most-specific overload (`plusAssign(element: T)` binds
/// the `Object` descriptor, `plusAssign(elements: Iterable)` the `Iterable` one).
fn meta_callable_aligns(f: &super::metadata::MetaFn, desc_params: &[Ty]) -> Option<(usize, usize)> {
    let off = f.is_extension as usize;
    let end = off + f.value_params.len();
    if end > desc_params.len() {
        return None;
    }
    let receiver_ok = !f.is_extension
        || match f.receiver_class {
            Some(rc) => meta_param_compat(Some(rc), &desc_params[0]),
            None => desc_params[0].is_reference(),
        };
    if !receiver_ok
        || !f
            .value_params
            .iter()
            .zip(&desc_params[off..end])
            .all(|(m, d)| meta_param_compat(m.ty, d))
    {
        return None;
    }
    let exact = f
        .value_params
        .iter()
        .zip(&desc_params[off..end])
        .filter(|(m, d)| meta_param_exact(m.ty, d))
        .count();
    Some((end, exact))
}

/// Pick the metadata function whose signature corresponds to the JVM method with `desc_params`, returning
/// `(kept-param end, index into `meta.fns`)`. Disambiguates OVERLOADS sharing a JVM name
/// (`any()` vs `any(predicate)`, `IntArray.any` vs `CharArray.any`) by receiver + value-parameter match,
/// preferring the longest alignment.
fn aligned_meta_index(
    meta: &ClassMeta,
    fn_name: &str,
    desc_params: &[Ty],
    desc_ret: &Ty,
) -> Option<(usize, usize)> {
    meta.by_jvm_name
        .get(fn_name)?
        .iter()
        .filter_map(|&i| {
            let f = &meta.fns[i];
            let (end, exact) = meta_callable_aligns(f, desc_params)?;
            // Return match disambiguates overloads that differ ONLY by return (`sum` → `sumOfInt`/
            // `sumOfLong`, same erased params). Soft tiebreaker: a concrete metadata return equal to the
            // descriptor's return wins, but a generic/type-parameter return (`class` None) or one that
            // erases differently (a value class vs its underlying) is left to the params match, so a sole
            // candidate still wins.
            let ret_match = f
                .ret_class
                .is_some_and(|rc| meta_param_compat(Some(rc), desc_ret));
            Some((end, exact, ret_match, i))
        })
        .max_by_key(|(end, exact, ret_match, _)| (*end, *exact, *ret_match))
        .map(|(end, _, _, i)| (end, i))
}

fn aligned_meta_callable<'a>(
    meta: &'a ClassMeta,
    fn_name: &str,
    desc_params: &[Ty],
    desc_ret: &Ty,
) -> Option<(usize, &'a super::metadata::MetaFn)> {
    aligned_meta_index(meta, fn_name, desc_params, desc_ret).map(|(end, i)| (end, &meta.fns[i]))
}

pub(super) fn metadata_return_info(class: Option<TypeName>, nullable: bool) -> ReturnInfo {
    ReturnInfo::new(nullable, class.map(kotlin_type_name_to_ty))
}

/// Per-class `@Metadata` cache: class internal name → Kotlin function names that participate in
/// `@OverloadResolutionByLambdaReturnType` (`sumOf`, …). The resolver derives and verifies the concrete
/// JVM method (`sumOfInt`/`sumOfLong`/…) from the lambda return type, so the cache only needs membership.
type LambdaReturnOverloads = std::collections::HashSet<String>;
type MetaOverloadCache =
    RefCell<crate::lru::LruCache<TypeName, std::rc::Rc<LambdaReturnOverloads>>>;

#[derive(Default)]
pub struct Classpath {
    entries: Vec<Entry>,
    // Two-level parsed-class cache: `local` is a per-thread L1 (cheap `RefCell`, no lock — serves the
    // hot repeated lookups), backed by `shared` — a process-global L2 (`RwLock`) so a class is PARSED
    // once across all rayon worker threads, not once per thread. L1 miss → L2 → parse.
    local_cache: RefCell<crate::lru::LruCache<TypeName, Option<std::sync::Arc<ClassInfo>>>>,
    cache: ClassCache,
    /// Open `ZipArchive` per jar path, so reading an entry is a central-directory hash lookup + inflate
    /// — NOT a re-parse of the whole central directory (which `zip::ZipArchive::new` does, thousands of
    /// entries for kotlin-stdlib). This is the classloader/javac strategy: parse each jar's directory
    /// once, then read class bytes lazily on demand. Profiling showed the per-read re-parse dominated
    /// type checking. Lives behind a `RefCell` (one `Classpath` per thread; never shared across threads).
    archives: RefCell<HashMap<PathBuf, zip::ZipArchive<File>>>,
    ext: RefCell<Option<std::sync::Arc<ExtIndex>>>,
    types: RefCell<Option<std::sync::Arc<TypeIndex>>>,
    /// The composed package table (`package NameId → PackageNode`, each node listing the jars that declare
    /// that package) — the merged classpath view name resolution walks. Composed once from the per-jar
    /// [`JarPackages`] (each cached per jar via [`EntryCache`]) and shared via `Arc` from a process-global
    /// cache keyed by the entry set, so a cp that adds one library reuses every other jar's catalog.
    pkg_tree: RefCell<Option<std::sync::Arc<PackageTree>>>,
    /// Lazily-built index of the JDK jimage: internal class-name id → [`JimageEntry`], so JDK class bytes
    /// can be seek-read (and inflated, for a compressed image) on demand. Shared via `Arc` from a
    /// process-global cache so the 146 MB parse happens once.
    jimage: RefCell<Option<(PathBuf, std::sync::Arc<JimageIndex>)>>,
    /// Cache of lazily-read method bodies (`(internal-name, name, descriptor) → MethodCode`), so the inline
    /// expander reads each inline function's body once even when it's called many times.
    bodies: RefCell<crate::lru::LruCache<(TypeName, String, String), Option<MethodCode>>>,
    /// Cache of each class's decoded `@Metadata` functions (facade parts merged) — the single decode the
    /// return-type / receiver / nullability / kept-param lookups all project over (see [`MetaFnsCache`]).
    meta_fns: MetaFnsCache,
    /// Cache of each class's `@Metadata` Kotlin-name → `@JvmName` overloads (see [`MetaOverloadCache`]).
    meta_overloads: MetaOverloadCache,
    /// Cache of resolved `LibraryType`s by global internal-name id. Kept on the reused-per-thread
    /// `Classpath` (NOT the per-compile `JvmLibraries`) so the import-driven `resolve_type` probing — which
    /// asks for the same stdlib types across thousands of snippets — warms across compiles instead of
    /// rebuilding each `LibraryType` (descriptor parses + `@Metadata` decodes) from cold every file.
    resolved_types:
        RefCell<crate::lru::LruCache<TypeName, Option<std::rc::Rc<crate::libraries::LibraryType>>>>,
    /// Parsed `.kotlin_builtins` fragments, keyed by package-name id (e.g. `kotlin`,
    /// `kotlin/collections`), each mapping class internal name → its supertypes + members. Built once
    /// per file on first use — the single source for BOTH the collection read-only/mutable hierarchy AND
    /// every builtin type's API. Empty if no stdlib is on the classpath.
    builtins: RefCell<HashMap<TypeName, std::rc::Rc<BuiltinsFile>>>,
    /// Resolved builtin member vectors, keyed by Kotlin internal class name. The raw builtins fragment is
    /// already cached, but mapping it to `LibraryMember`s also resolves JVM owners/interface flags and
    /// allocates descriptors. `resolve_type` asks for these repeatedly during member/subtype lookup.
    builtin_members:
        RefCell<crate::lru::LruCache<TypeName, std::rc::Rc<Vec<crate::libraries::LibraryMember>>>>,
    /// Rebuilt ext/top-level candidates per method name (the lazy [`ExtIndex`]'s `by_name` gives WHERE;
    /// this memoizes the actual rebuilt records so a hot stdlib name isn't re-walked on every query). Two
    /// levels, like the parsed-class cache: `ext_l1` is a per-thread `RefCell` — a CHEAP borrow on the hot
    /// resolver path (`find_extensions` is called per call site) — holding `Arc`s shared from `ext_candidates`,
    /// the process-global L2 where the one-time rebuild lives. Both hold only QUERIED names (the working set).
    ext_l1: RefCell<crate::lru::LruCache<String, std::sync::Arc<ExtByName>>>,
    ext_candidates: ExtCandCache,
    /// The spec's top-level memo: `fqn → ResolvedSymbols` (the namespace record — classifier + callables).
    /// The single result cache of the classpath `SymbolSource`: `resolve_symbols(fqn)` is composed once
    /// per name and reused across the compile (the per-(jar,package) parses are the intermediate caches).
    /// An LRU bounded to the queried working set.
    symbols_memo:
        RefCell<crate::lru::LruCache<TypeName, std::rc::Rc<crate::libraries::ResolvedSymbols>>>,
    /// Process-unique identity for this `Classpath`, assigned at construction. Caches keyed by a
    /// `Classpath` (e.g. the per-classpath library seed) MUST key on this — NOT on the `Rc<Classpath>`
    /// pointer address, which a freed-then-reallocated `Classpath` can reuse, yielding a false cache hit
    /// that serves a DIFFERENT classpath's data (e.g. a cross-module class going unresolved).
    id: u64,
}

/// Current process resident-set size in KiB from Linux `/proc/self/status` (`VmRSS`, already in KiB),
/// for memory profiling. `0` if unavailable (non-Linux, or the file can't be read).
pub fn process_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

impl Classpath {
    pub fn new(paths: Vec<PathBuf>) -> Classpath {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let entries: Vec<Entry> = paths
            .into_iter()
            .map(|p| {
                let is_archive = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("jar") || e.eq_ignore_ascii_case("zip"))
                    .unwrap_or(false);
                // A JDK jimage is conventionally `<jdk>/lib/modules` (a file named `modules`).
                let is_jimage = p.is_file() && p.file_name().map_or(false, |n| n == "modules");
                if is_jimage {
                    Entry::Jimage(p)
                } else if is_archive {
                    Entry::Jar(p)
                } else {
                    Entry::Dir(p)
                }
            })
            .collect();
        let cache_key: Vec<PathBuf> = entries.iter().map(|e| e.path().to_path_buf()).collect();
        // Per-cache LRU caps (entry counts). Sized so the warm working set of common stdlib/JDK classes
        // and their call queries stays resident across compiles, while one-off classes evict — bounding
        // per-thread memory instead of growing toward the full JDK. Override all at once with
        // `KRUSTY_CACHE_CAP`. `CLASS_CAP`/`FN_CAP` are the two large ones (parsed classes, function sets).
        const CLASS_CAP: usize = 4096;
        const FN_CAP: usize = 8192;
        const META_CAP: usize = 4096;
        const BODY_CAP: usize = 2048;
        Classpath {
            entries,
            local_cache: RefCell::new(crate::lru::LruCache::new(CLASS_CAP)),
            cache: global_class_cache(&cache_key),
            archives: RefCell::new(HashMap::new()),
            ext: RefCell::new(None),
            types: RefCell::new(None),
            pkg_tree: RefCell::new(None),
            jimage: RefCell::new(None),
            bodies: RefCell::new(crate::lru::LruCache::new(BODY_CAP)),
            meta_fns: RefCell::new(crate::lru::LruCache::new(META_CAP)),
            meta_overloads: RefCell::new(crate::lru::LruCache::new(META_CAP)),
            resolved_types: RefCell::new(crate::lru::LruCache::new(CLASS_CAP)),
            builtins: RefCell::new(HashMap::new()),
            builtin_members: RefCell::new(crate::lru::LruCache::new(META_CAP)),
            ext_l1: RefCell::new(crate::lru::LruCache::new(FN_CAP)),
            ext_candidates: global_ext_candidates(&cache_key),
            symbols_memo: RefCell::new(crate::lru::LruCache::new(FN_CAP)),
            id,
        }
    }

    /// Process-unique identity assigned at construction — a stable cache key for per-classpath caches
    /// (see the `id` field). Unlike an `Rc<Classpath>` pointer, this never aliases a freed classpath.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// A one-line snapshot of every cache's entry count — for memory profiling (`KRUSTY_MEM_REPORT`). The
    /// per-`Classpath` caches (`L1_class`/`meta*`/`bodies`/`builtin`) are LRU-bounded, so they
    /// plateau at their caps; the shared `L2_class` map and the `jimage`/`type`/`ext` INDEXES are the
    /// library-sized structures (the jimage names every JDK class) — the ones to watch if RSS is high.
    pub fn cache_report(&self) -> String {
        let jimage = self
            .jimage
            .borrow()
            .as_ref()
            .map_or(0, |(_, i)| i.by_name.len());
        let types = self
            .types
            .borrow()
            .as_ref()
            .map_or(0, |i| i.type_aliases.len());
        let ext = self
            .ext
            .borrow()
            .as_ref()
            .map_or(0, |i| i.by_name.len() + i.by_recv_owners.len());
        let pkgtree = self
            .pkg_tree
            .borrow()
            .as_ref()
            .map_or(0, |t| t.package_count());
        format!(
            "classpath#{} L1_class={} L2_class={} meta_fns={} meta_ovl={} bodies={} builtin={} | \
             jimage={} type={} ext={} pkgtree={}",
            self.id,
            self.local_cache.borrow().len(),
            self.cache.len(),
            self.meta_fns.borrow().len(),
            self.meta_overloads.borrow().len(),
            self.bodies.borrow().len(),
            self.builtin_members.borrow().len(),
            jimage,
            types,
            ext,
            pkgtree,
        )
    }

    /// The composed classpath package table (`package NameId → node`, each node listing the jars that
    /// declare that package), built once from the per-jar [`JarPackages`] and shared via `Arc`. The merged
    /// view resolves `tree.node_for("kotlin/collections")` to the jars to consult. Cached per-instance and
    /// process-globally by the entry set.
    pub fn package_tree(&self) -> std::sync::Arc<PackageTree> {
        if let Some(t) = self.pkg_tree.borrow().as_ref() {
            return t.clone();
        }
        let key: Vec<PathBuf> = self
            .entries
            .iter()
            .map(|e| e.path().to_path_buf())
            .collect();
        if let Some(t) = global_pkg_tree_cache().lock().unwrap().get(&key) {
            *self.pkg_tree.borrow_mut() = Some(t.clone());
            return t.clone();
        }
        let parts: Vec<std::sync::Arc<JarPackages>> = self
            .entries
            .iter()
            .map(|e| global_jar_packages().get_or_build(e.path(), || build_jar_packages(e)))
            .collect();
        let tree = std::sync::Arc::new(compose_package_tree(&parts));
        global_pkg_tree_cache()
            .lock()
            .unwrap()
            .insert(key, tree.clone());
        *self.pkg_tree.borrow_mut() = Some(tree.clone());
        tree
    }

    /// Memoized `resolve_type` result for `internal` (the outer `Option` = cached-vs-not; the inner =
    /// resolved-vs-absent). Warm across compiles because this `Classpath` is reused per worker thread.
    pub fn cached_library_type(
        &self,
        internal: &str,
    ) -> Option<Option<std::rc::Rc<crate::libraries::LibraryType>>> {
        self.cached_library_type_name(type_name(internal))
    }

    pub fn cached_library_type_name(
        &self,
        internal: TypeName,
    ) -> Option<Option<std::rc::Rc<crate::libraries::LibraryType>>> {
        self.resolved_types.borrow_mut().get(&internal).cloned()
    }

    pub fn cache_library_type(
        &self,
        internal: &str,
        ty: Option<std::rc::Rc<crate::libraries::LibraryType>>,
    ) {
        self.cache_library_type_name(type_name(internal), ty);
    }

    pub fn cache_library_type_name(
        &self,
        internal: TypeName,
        ty: Option<std::rc::Rc<crate::libraries::LibraryType>>,
    ) {
        self.resolved_types.borrow_mut().insert(internal, ty);
    }

    /// The decoded `@Metadata` function lookups for `internal` (facade parts merged), decoded once and
    /// cached. The single `d1` decode that `meta_functions`/`metadata_call_facts` all project over.
    fn class_meta(&self, internal: &str) -> std::rc::Rc<ClassMeta> {
        self.class_meta_name(type_name(internal))
    }

    fn class_meta_name(&self, internal_id: TypeName) -> std::rc::Rc<ClassMeta> {
        if let Some(m) = self.meta_fns.borrow_mut().get(&internal_id) {
            cache_stat!(meta_fns, true);
            return m.clone();
        }
        cache_stat!(meta_fns, false);
        let ci = self.find_name(internal_id);
        let mut fns = ci
            .as_ref()
            .map(|c| super::metadata::package_functions(c))
            .unwrap_or_default();
        let mut suspend_names: HashSet<String> = fns
            .iter()
            .filter(|f| f.is_suspend)
            .map(|f| f.kotlin_name.clone())
            .collect();
        if let Some(ci) = &ci {
            suspend_names.extend(
                super::metadata::class_functions(ci)
                    .into_iter()
                    .filter(|f| f.is_suspend)
                    .map(|f| f.kotlin_name),
            );
        }
        // A multifile FACADE has no function metadata of its own — its `d1` lists the PART class names,
        // which hold the functions; merge them in (the parts' `d1` is decoded once here, not per lookup).
        if fns.is_empty() {
            if let Some(ci) = &ci {
                for part in &ci.kotlin_d1 {
                    if let Some(pci) = self.find(part) {
                        let mut part_fns = super::metadata::package_functions(&pci);
                        suspend_names.extend(
                            part_fns
                                .iter()
                                .filter(|f| f.is_suspend)
                                .map(|f| f.kotlin_name.clone()),
                        );
                        suspend_names.extend(
                            super::metadata::class_functions(&pci)
                                .into_iter()
                                .filter(|f| f.is_suspend)
                                .map(|f| f.kotlin_name),
                        );
                        fns.append(&mut part_fns);
                    }
                }
            }
        }
        let mut by_jvm_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, f) in fns.iter().enumerate() {
            by_jvm_name.entry(f.jvm_name.clone()).or_default().push(i);
        }
        let meta = std::rc::Rc::new(ClassMeta {
            by_jvm_name,
            suspend_names,
            fns: fns.into(),
        });
        self.meta_fns.borrow_mut().insert(internal_id, meta.clone());
        meta
    }

    /// Every `@Metadata` function of `internal` (a facade's PART classes merged), decoded once and
    /// cached — the single source the metadata-primary `MetaFn` lookups share. Use this instead of
    /// re-calling `package_functions` + re-merging the facade parts at each call site.
    pub fn meta_functions(&self, internal: &str) -> std::rc::Rc<[super::metadata::MetaFn]> {
        self.class_meta(internal).fns.clone()
    }

    pub fn meta_functions_name(
        &self,
        internal: TypeName,
    ) -> std::rc::Rc<[super::metadata::MetaFn]> {
        self.class_meta_name(internal).fns.clone()
    }

    /// The metadata-primary [`GenericSig`] for the `internal.jvm_name` overload corresponding to the JVM
    /// method with `desc_params`. kotlinc omits the `method_signature` extension when it equals the
    /// computed default, so the correct overload is picked by aligning the metadata signature to the
    /// descriptor (receiver + value parameters) — the SAME selection the call-fact lookup uses, so both
    /// agree. Outer `None` means no metadata function by this JVM name, so the caller may use JVM
    /// `Signature`; inner `None` means metadata owns the callable but has no usable generic signature.
    pub fn aligned_generic_sig_name(
        &self,
        internal: TypeName,
        jvm_name: &str,
        desc_params: &[Ty],
        desc_ret: &Ty,
    ) -> Option<Option<crate::libraries::GenericSig>> {
        let meta = self.class_meta_name(internal);
        meta.by_jvm_name.contains_key(jvm_name).then(|| {
            aligned_meta_index(&meta, jvm_name, desc_params, desc_ret)
                .and_then(|(_, idx)| meta.fns.get(idx))
                .and_then(|f| f.generic_sig.clone())
        })
    }

    /// The SOURCE value-parameter types of `internal.fn_name` from `@Metadata`, as `Ty`s — the signature
    /// a CALL is matched against. `@Metadata` records only the source `value_parameter`s, so this DROPS
    /// the synthetic params the JVM descriptor appends (a `suspend` Continuation, a `@Composable`
    /// Composer/int) — the same role `strip_continuation_param` played for suspend, now generic. A
    /// function-type param maps to semantic `Ty::Fun` so a lambda arg fits structurally; a type-parameter
    /// param erases to `kotlin/Any` (accepts anything). `None` when the
    /// class has no `@Metadata` entry for `fn_name` (a Java method, a synthetic) — the caller then keeps
    /// the descriptor params unchanged.
    /// The descriptor-aligned source call facts for top-level/static `internal.fn_name`: kept source
    /// arity, named/default call shape, receiver-lambda annotations, materialization flags, and return
    /// metadata. Everything is projected from ONE `@Metadata` callable, so overloads cannot drift across
    /// parallel lookups.
    pub fn metadata_call_facts(
        &self,
        internal: &str,
        fn_name: &str,
        desc_params: &[Ty],
        desc_ret: &Ty,
        extension: bool,
    ) -> MetadataCallFacts {
        self.metadata_call_facts_name(
            type_name(internal),
            fn_name,
            desc_params,
            desc_ret,
            extension,
        )
    }

    pub fn metadata_call_facts_name(
        &self,
        internal: TypeName,
        fn_name: &str,
        desc_params: &[Ty],
        desc_ret: &Ty,
        extension: bool,
    ) -> MetadataCallFacts {
        let meta = self.class_meta_name(internal);
        let Some((end, c)) = aligned_meta_callable(&meta, fn_name, desc_params, desc_ret) else {
            return MetadataCallFacts::fallback(if extension {
                CallSig::default()
            } else {
                CallSig::metadata_plain(desc_params.len())
            });
        };
        let names = c.value_params.iter().map(|p| p.name.clone()).collect();
        let defaults = c.value_params.iter().map(|p| p.has_default).collect();
        MetadataCallFacts {
            kept_params: Some(end),
            call_sig: if extension {
                CallSig::metadata_extension(end, names, defaults)
            } else {
                CallSig::metadata_top_level(
                    end,
                    names,
                    defaults,
                    c.value_params
                        .iter()
                        .map(|p| p.recv_fun_receiver.map(Ty::obj_name))
                        .collect(),
                    c.value_params.iter().map(|p| p.recv_fun).collect(),
                    c.value_params.iter().map(|p| p.materialized).collect(),
                )
            },
            ret: metadata_return_info(c.ret_class, c.ret_nullable),
        }
    }

    /// The source-level call and return facts of class MEMBER `internal.jvm_name/arity`, from the class's
    /// own `@Metadata` function record. Names, default flags, return classifier, and nullability come
    /// from the SAME member record, so a data-class `copy`, value-class-mangled member, or `suspend`
    /// return cannot drift across separate metadata lookups.
    pub fn metadata_member_call_facts_name(
        &self,
        internal: TypeName,
        jvm_name: &str,
        arity: usize,
    ) -> MetadataCallFacts {
        let Some(ci) = self.find_name(internal) else {
            return MetadataCallFacts::fallback(CallSig::metadata_plain(arity));
        };
        let Some(f) = super::metadata::class_functions(&ci)
            .into_iter()
            .find(|f| f.jvm_name == jvm_name && f.value_params.len() == arity)
        else {
            return MetadataCallFacts::fallback(CallSig::metadata_plain(arity));
        };
        MetadataCallFacts {
            kept_params: None,
            call_sig: f.member_call_sig(),
            ret: metadata_return_info(f.ret_class, f.ret_nullable),
        }
    }

    /// A facade class's lambda-return-overload Kotlin names, cached (part-merged for a multifile facade).
    pub fn lambda_return_overloads(&self, internal: &str) -> std::rc::Rc<LambdaReturnOverloads> {
        let internal_id = type_name(internal);
        if let Some(m) = self.meta_overloads.borrow_mut().get(&internal_id) {
            return m.clone();
        }
        // Overloads of one Kotlin name are split across the multifile facade's PART classes (the
        // `Int`/`Long`/`Double` `sumOf` in one part, `UInt`/`ULong` in another). The facade EXTENDS its
        // parts, so union every class's own metadata up the superclass chain — exactly how the extension
        // index reaches the part methods (a part isn't listed in the facade's `d1`).
        let mut names = LambdaReturnOverloads::new();
        let mut cur = Some(internal_id);
        let mut seen = std::collections::HashSet::new();
        while let Some(cn) = cur {
            if !seen.insert(cn) {
                break;
            }
            let Some(ci) = self.find_name(cn) else { break };
            for f in self.meta_functions_name(cn).iter() {
                if f.jvm_desc.is_some() && f.ret_class.is_some() {
                    names.insert(f.kotlin_name.clone());
                }
            }
            cur = ci.super_class;
        }
        let rc = std::rc::Rc::new(names);
        self.meta_overloads
            .borrow_mut()
            .insert(internal_id, rc.clone());
        rc
    }

    /// Every distinct owner (facade) that declares a static method whose first parameter matches
    /// `receiver_desc` — the facades to consult for a Kotlin-name resolution (`sumOf`).
    pub fn find_extension_owners(&self, receiver_desc: &str) -> Vec<TypeName> {
        self.ensure_ext_index();
        let ext = self.ext.borrow().as_ref().cloned();
        ext.and_then(|idx| {
            idx.by_recv_owners.get(receiver_desc).map(|owners| {
                owners
                    .iter()
                    .map(|&id| type_name_from(&idx.owner_names, id))
                    .collect()
            })
        })
        .unwrap_or_default()
    }

    /// Rebuild the [`ExtCandidate`]s a facade/part `root` contributes for `name` — the lazy counterpart of
    /// the old eager index. Walks `root`'s super-class chain (each `ClassInfo` served from the L1/L2 cache),
    /// collecting matching statics; `public` mirrors the eager filter (a non-public root's public statics
    /// are the `@InlineOnly` splice-only candidates the inliner may select but resolution never emits).
    fn rebuild_ext_candidate_records(
        &self,
        owner: NameId,
        root: &str,
        name: &str,
    ) -> Vec<ExtCandidateRecord> {
        let mut out = Vec::new();
        let Some(root_ci) = self.find(root) else {
            return out;
        };
        let root_public = root_ci.is_public();
        let mut cur = Some(root.to_string());
        let mut visited = std::collections::HashSet::new();
        while let Some(cn) = cur {
            if !visited.insert(cn.clone()) {
                break;
            }
            let Some(ci) = self.find(&cn) else { break };
            for m in &ci.methods {
                // Static methods of this name only — never `<init>`/`<clinit>` (the eager scan excluded
                // `<`-prefixed names; a real call name never starts with `<`, so this only hardens the path).
                if m.name != name || !m.is_static() || m.name.starts_with('<') {
                    continue;
                }
                if !root_public && m.is_public() {
                    continue;
                }
                let Some((_, ret_desc)) = descriptor_parts(&m.descriptor) else {
                    continue;
                };
                out.push(ExtCandidateRecord {
                    owner,
                    name: m.name.clone(),
                    descriptor: m.descriptor.clone(),
                    ret_desc,
                    signature: m.signature.clone(),
                    public: root_public && m.is_public(),
                });
            }
            cur = ci.super_class();
        }
        out
    }

    /// A parsed `.kotlin_builtins` fragment by package id (class internal-name id → supertypes+members),
    /// read once and cached. The single builtins entry point — both the collection hierarchy and a
    /// type's member API derive from it.
    fn builtins_file_for_package(&self, package: TypeName) -> std::rc::Rc<BuiltinsFile> {
        if let Some(m) = self.builtins.borrow().get(&package) {
            return m.clone();
        }
        let path = Self::builtins_path_for_package(package);
        let mut map = HashMap::new();
        for e in &self.entries {
            if let Entry::Jar(j) = e {
                if let Some(bytes) = self.jar_entry(j, &path) {
                    map = super::metadata::parse_builtins(&bytes);
                    break;
                }
            }
        }
        let rc = std::rc::Rc::new(BuiltinsFile::from_classes(map));
        self.builtins.borrow_mut().insert(package, rc.clone());
        rc
    }

    /// The `.kotlin_builtins` fragment path for a package, mirroring kotlinc's
    /// `BuiltInSerializerProtocol.getBuiltInsFilePath`: `kotlin` → `kotlin/kotlin.kotlin_builtins`,
    /// `kotlin/collections` → `kotlin/collections/collections.kotlin_builtins`.
    fn builtins_path_for_package(package: TypeName) -> String {
        let pkg = package.render();
        let last = package.segment();
        format!("{pkg}/{last}.kotlin_builtins")
    }

    fn builtins_package_for(internal: TypeName) -> TypeName {
        internal.parent().unwrap_or_else(|| type_name(""))
    }

    /// The parsed `collections.kotlin_builtins` fragment (the Kotlin collection hierarchy lives here).
    fn collection_builtins(&self) -> std::rc::Rc<BuiltinsFile> {
        self.builtins_file_for_package(type_name("kotlin/collections"))
    }

    /// Kotlin BUILTIN members (`String.length`, `List.get`, `Number.toInt`, …) as regular
    /// `LibraryMember` facts. The source name stays in `name`; JVM realization details stay in the JVM
    /// backend/provider and descriptor data.
    pub fn builtin_members(&self, internal: &str) -> Vec<crate::libraries::LibraryMember> {
        let internal_id = type_name(internal);
        if let Some(members) = self.builtin_members.borrow_mut().get(&internal_id) {
            cache_stat!(builtin_members, true);
            return members.as_ref().clone();
        }
        cache_stat!(builtin_members, false);
        let f = self.builtins_file_for_package(Self::builtins_package_for(internal_id));
        let members: Vec<_> = f
            .get(internal)
            .map(|class| {
                class.members.iter().map(|m| {
                    let pdesc: String = m.params.iter().map(BuiltinType::descriptor).collect();
                    let descriptor = format!("({pdesc}){}", m.ret.descriptor());
                    let ret = m.ret.ty();
                    let physical_ret = if m.ret.is_class() {
                        ret
                    } else {
                        Ty::obj("kotlin/Any")
                    };
                    // The owner's JVM class: the kotlin↔JVM map (`kotlin/String` → `java/lang/String`), and for the
                    // non-collection mapped builtins (`kotlin/CharSequence` → `java/lang/CharSequence`, …) the
                    // emit-only simple-name mapping — the member virtual-dispatches on that JVM type.
                    let mapped = crate::jvm::jvm_class_map::to_jvm_internal(internal);
                    let owner = if mapped != internal {
                        mapped.to_string()
                    } else if let Some(j) = internal
                        .strip_prefix("kotlin/")
                        .filter(|s| !s.contains('/'))
                        .and_then(crate::jvm::jvm_class_map::kotlin_builtin_to_jvm)
                    {
                        j.to_string()
                    } else {
                        internal.to_string()
                    };
                    // Interface dispatch: prefer the real class flag, but fall back to the curated mapped-builtin
                    // answer when the `.class` reader can't load the owner (a JDK jimage krusty can't decode).
                    let is_iface = self
                        .find(&owner)
                        .map(|ci| ci.is_interface())
                        .or_else(|| {
                            crate::jvm::jvm_class_map::jvm_mapped_builtin_is_interface(&owner)
                        })
                        .unwrap_or(false);
                    // The READ direction of the property-accessor mapping (the WRITE direction is the
                    // bridge synthesis in `names::collection_property_stub_name`, reused here): a special
                    // `JavaToKotlinClassMap` collection stub (`keys` → `keySet`), the `CharSequence.length`
                    // plain method, else the JavaBean getter (`is`-prefix kept, otherwise `get<Name>`).
                    let member_name = if m.is_property {
                        if let Some(stub) =
                            crate::jvm::names::collection_property_stub_name(&m.name)
                        {
                            stub.to_string()
                        } else if m.name == "length" {
                            m.name.clone()
                        } else {
                            crate::jvm::names::property_getter_name(&m.name)
                        }
                    } else {
                        m.name.clone()
                    };
                    crate::libraries::LibraryMember {
                        name: member_name,
                        owner: Some(type_name(&owner)),
                        physical_name: None,
                        params: m.params.iter().map(BuiltinType::ty).collect(),
                        ret,
                        // The declared return nullability from the `.kotlin_builtins` `Type.nullable`
                        // flag (`Map.get(K): V?`) — the JVM descriptor erases it.
                        ret_nullable: m.ret_nullable,
                        physical_ret,
                        descriptor,
                        signature: None,
                        generic_sig: None,
                        is_interface: is_iface,
                        inline: crate::libraries::InlineKind::None,
                        suspend: false,
                        // Builtin (`.kotlin_builtins`) members are all public API.
                        visibility: crate::libraries::Visibility::Public,
                        // Builtin members carry no source parameter-name metadata.
                        call_sig: crate::libraries::CallSig::default(),
                    }
                })
            })
            .into_iter()
            .flatten()
            .collect();
        self.builtin_members
            .borrow_mut()
            .insert(internal_id, std::rc::Rc::new(members.clone()));
        members
    }

    /// Whether the Kotlin builtin `internal` declares its function member `name`/`arity` with a NULLABLE
    /// return (`kotlin/collections/Map.get(K): V?`). A generic-return member is dropped from
    /// `builtin_members` (its return is a bare type parameter), and the member that actually resolves such
    /// a call is the erased classpath method (`java/util/Map.get` → `Object`) which carries no Kotlin
    /// nullability — so the builtin's `Type.nullable` flag is the only surviving record. `false` when no
    /// such member/builtin is recorded.
    pub fn builtin_member_ret_nullable(&self, internal: &str, name: &str, arity: usize) -> bool {
        let internal_id = type_name(internal);
        self.builtins_file_for_package(Self::builtins_package_for(internal_id))
            .get_name(internal_id)
            .is_some_and(|c| {
                c.nullable_member_returns
                    .iter()
                    .any(|(n, a)| n == name && *a == arity)
            })
    }

    /// Whether the Kotlin builtin `internal` declares `name` as a PROPERTY (not a function) in its
    /// `.kotlin_builtins` fragment (`CharSequence.length`, `Collection.size`). Distinguishes a property
    /// reference (`s::length` → `KProperty0`) from a zero-arg method reference (`it::next` → function).
    pub fn builtin_member_is_property(&self, internal: &str, name: &str) -> bool {
        let internal_id = type_name(internal);
        self.builtins_file_for_package(Self::builtins_package_for(internal_id))
            .get_name(internal_id)
            .is_some_and(|c| c.members.iter().any(|m| m.name == name && m.is_property))
    }

    pub fn builtin_member_is_property_name(&self, internal: TypeName, name: &str) -> bool {
        self.builtins_file_for_package(Self::builtins_package_for(internal))
            .get_name(internal)
            .is_some_and(|c| c.members.iter().any(|m| m.name == name && m.is_property))
    }

    /// Direct supertypes declared in `.kotlin_builtins` for a Kotlin builtin class.
    pub fn builtin_supertypes(&self, internal: &str) -> Vec<String> {
        let internal_id = type_name(internal);
        self.builtins_file_for_package(Self::builtins_package_for(internal_id))
            .get_name(internal_id)
            .map(|c| c.supertypes.iter_rendered().collect())
            .unwrap_or_default()
    }

    pub fn builtin_supertypes_name(&self, internal: TypeName) -> TypeNameList {
        self.builtins_file_for_package(Self::builtins_package_for(internal))
            .get_name(internal)
            .map(|c| c.supertypes.clone())
            .unwrap_or_default()
    }

    /// The target internal name of the classpath `typealias` named `internal` (full name, e.g.
    /// `kotlin/collections/ArrayList` → `java/util/ArrayList`), or `None` if `internal` is not an alias.
    pub fn type_alias_target(&self, internal: &str) -> Option<String> {
        self.type_alias_target_name(type_name(internal))
            .map(TypeName::render)
    }

    pub fn type_alias_target_name(&self, internal: TypeName) -> Option<TypeName> {
        let idx = self.scan_types();
        idx.type_aliases.get(&internal).copied()
    }

    /// Whether `internal` is a Kotlin BUILTIN declared in a `.kotlin_builtins` fragment (`kotlin/Number`,
    /// `kotlin/collections/List`, …), and if so whether it is an interface. `None` = not a builtin. Lets
    /// `resolve_type` report a builtin whose JVM class is absent (a no-JDK compile) from the builtins data,
    /// with the right class-vs-interface kind for member-invoke codegen.
    pub fn builtin_is_interface(&self, internal: &str) -> Option<bool> {
        let internal_id = type_name(internal);
        self.builtins_file_for_package(Self::builtins_package_for(internal_id))
            .get_name(internal_id)
            .map(|c| c.is_interface)
    }

    pub fn builtin_is_interface_name(&self, internal: TypeName) -> Option<bool> {
        self.builtins_file_for_package(Self::builtins_package_for(internal))
            .get_name(internal)
            .map(|c| c.is_interface)
    }

    /// Whether `internal` names a type in the Kotlin collection hierarchy (`collections.kotlin_builtins`)
    /// — i.e. one whose read-only/mutable identity is known here. A platform `java/util/List` or a user
    /// class is NOT (the front end never produces the former for a Kotlin collection; both keep their
    /// JVM-erased resolution).
    pub fn is_kotlin_collection(&self, internal: &str) -> bool {
        self.collection_builtins().contains_key(internal)
    }

    pub fn is_kotlin_collection_name(&self, internal: TypeName) -> bool {
        self.collection_builtins().contains_key_name(internal)
    }

    /// Whether `sub` is, or transitively is a subtype of, `sup` within the Kotlin collection hierarchy
    /// read from `collections.kotlin_builtins` (`MutableList <: MutableCollection`; `List` is NOT). The
    /// generic subtype query behind extension applicability — `MutableCollection.plusAssign` applies to a
    /// `MutableList` receiver but not a read-only `List`, exactly as kotlinc's overload resolution.
    pub fn kotlin_subtype(&self, sub: &str, sup: &str) -> bool {
        self.collection_builtins().is_subtype(sub, sup)
    }

    pub fn kotlin_subtype_name(&self, sub: TypeName, sup: TypeName) -> bool {
        self.collection_builtins().is_subtype_name(sub, sup)
    }

    pub fn empty() -> Classpath {
        Classpath::default()
    }

    /// Scan all classpath entries and return the full type index (class names + type aliases).
    /// Cached per-instance after the first call, and **process-globally** keyed by the entry paths —
    /// so scanning the JDK jimage (the whole `java.base`) happens once per process, not once per
    /// compiled file (which dominated box-suite wall time).
    /// The classpath's type index, shared via `Arc` so per-file callers pay a pointer bump, not a
    /// deep clone of the (large) class-name/alias maps. Cached per-instance and process-globally.
    pub fn scan_types(&self) -> std::sync::Arc<TypeIndex> {
        if let Some(idx) = self.types.borrow().as_ref() {
            return idx.clone();
        }
        let key: Vec<PathBuf> = self
            .entries
            .iter()
            .map(|e| e.path().to_path_buf())
            .collect();
        if let Some(idx) = global_type_cache().lock().unwrap().get(&key) {
            *self.types.borrow_mut() = Some(idx.clone());
            return idx.clone();
        }
        // Compose from per-ENTRY alias tables. Each entry's scan (parse every `*Kt` facade for type
        // aliases) is built ONCE via `EntryCache` — which holds its map lock across the build — and shared
        // by every classpath that includes the jar. So the expensive scan no longer races across all
        // worker threads on cold start (it dominated `resolve_type` in the flamegraph via
        // `type_alias_target`); only the cheap map merge runs per classpath. This mirrors the ext index's
        // per-entry composition (d8bbc91).
        let mut idx = TypeIndex::default();
        for e in &self.entries {
            let part = global_entry_types().get_or_build(e.path(), || build_entry_types(e));
            for (&alias, &target) in &part.type_aliases {
                // First entry on the classpath wins — kotlinc/java class-resolution order (and this doc's
                // "first hit" invariant). The old inline scan `insert`ed in entry order, so a LATER jar
                // overwrote an earlier one (last-wins); that was a latent divergence, masked only because
                // no two corpus jars declare the same alias (box conformance stays FAIL:0 either way).
                idx.type_aliases.entry(alias).or_insert(target);
            }
        }
        let idx = std::sync::Arc::new(idx);
        global_type_cache().lock().unwrap().insert(key, idx.clone());
        *self.types.borrow_mut() = Some(idx.clone());
        idx
    }

    /// Seek-read a class's bytes from the JDK jimage via the lazily-built index. A "zip"-compressed
    /// resource (the JetBrains Runtime, or any `jlink --compress` image) is wrapped in a 29-byte
    /// `CompressedResourceHeader` (little-endian: magic `0xCAFEFAFA`, then `size`/`uncompressed_size`
    /// i64s, decompressor name/config offsets, an `is_terminal` byte) before a zlib Deflate stream;
    /// inflate it. The `compressed` flag is set by the indexer ONLY when the decompressor is exactly
    /// "zip", so a resource compressed by another scheme is left as-is (and fails to parse → unresolved)
    /// rather than blindly inflated.
    fn jimage_bytes(&self, internal: &str) -> Option<Vec<u8>> {
        self.ensure_jimage_index();
        let guard = self.jimage.borrow();
        let (path, index) = guard.as_ref()?;
        let id = index.names.get(internal)?;
        let &(offset, size, compressed) = index.by_name.get(&id)?;
        use std::io::{Read, Seek, SeekFrom};
        let mut f = File::open(path).ok()?;
        f.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = vec![0u8; size];
        f.read_exact(&mut buf).ok()?;
        // A compressed resource carries a `CompressedResourceHeader` (magic `0xCAFEFAFA`, little-endian
        // `[FA FA FE CA]`); inflate its zlib payload past the 29-byte header. The magic confirms the "zip"
        // decompressor (the build stores `compressed` from the table; this is the content-side check it
        // used to do eagerly) — a resource without it is returned as-is rather than mis-inflated.
        if compressed && buf.len() >= 29 && buf[0..4] == [0xFA, 0xFA, 0xFE, 0xCA] {
            let unc = u64::from_le_bytes(buf[12..20].try_into().ok()?) as usize;
            // The jimage is a trusted local JDK file, but cap the pre-allocation hint anyway — a real
            // `.class` is far under this, and `read_to_end` grows past it if ever needed.
            let mut out = Vec::with_capacity(unc.min(16 * 1024 * 1024));
            flate2::read::ZlibDecoder::new(&buf[29..])
                .read_to_end(&mut out)
                .ok()?;
            return Some(out);
        }
        Some(buf)
    }

    fn ensure_jimage_index(&self) {
        if self.jimage.borrow().is_some() {
            return;
        }
        let path = self.entries.iter().find_map(|e| {
            if let Entry::Jimage(p) = e {
                Some(p.clone())
            } else {
                None
            }
        });
        let entry = match path {
            Some(p) => {
                let mut g = global_jimage_cache().lock().unwrap();
                let idx = match g.get(&p) {
                    Some(i) => i.clone(),
                    None => {
                        let i = std::sync::Arc::new(build_jimage_index(&p).unwrap_or_default());
                        g.insert(p.clone(), i.clone());
                        i
                    }
                };
                (p, idx)
            }
            None => (PathBuf::new(), std::sync::Arc::new(JimageIndex::default())),
        };
        *self.jimage.borrow_mut() = Some(entry);
    }

    /// Read one entry's bytes from `jar`, reusing a cached open `ZipArchive` so the central directory is
    /// parsed once per jar rather than per read. Returns `None` if the jar or entry is absent (an absent
    /// entry is a cheap hash miss on the already-parsed directory).
    fn jar_entry(&self, jar: &Path, name: &str) -> Option<Vec<u8>> {
        let mut archives = self.archives.borrow_mut();
        let archive = match archives.get_mut(jar) {
            Some(a) => a,
            None => {
                let f = File::open(jar).ok()?;
                let a = zip::ZipArchive::new(f).ok()?;
                archives.entry(jar.to_path_buf()).or_insert(a)
            }
        };
        let mut entry = archive.by_name(name).ok()?;
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut buf).ok()?;
        Some(buf)
    }

    pub fn find(&self, internal: &str) -> Option<std::sync::Arc<ClassInfo>> {
        self.find_name(type_name(internal))
    }

    pub fn find_name(&self, internal: TypeName) -> Option<std::sync::Arc<ClassInfo>> {
        // The front end names built-in types in Kotlin terms (`kotlin/Any`); a classpath artifact is
        // a real JVM class, so map to the JVM name (`java/lang/Object`) before looking it up. The parsed
        // class is shared behind an `Arc`: L1↔L2 and every caller clone is a refcount bump, never a deep
        // copy of the (large) `ClassInfo`.
        let internal_id = super::jvm_class_map::to_jvm_type_name(internal);
        // L1: per-thread, no lock.
        if let Some(hit) = self.local_cache.borrow_mut().get(&internal_id) {
            cache_stat!(l1_class, true);
            return hit.clone();
        }
        cache_stat!(l1_class, false);
        // L2: process-global, shared across threads — a class parsed by ANY thread is reused here.
        if let Some(hit) = self
            .cache
            .classes
            .read()
            .unwrap()
            .get(&internal_id)
            .cloned()
        {
            cache_stat!(l2_class, true);
            self.local_cache
                .borrow_mut()
                .insert(internal_id, hit.clone());
            return hit;
        }
        cache_stat!(l2_class, false);
        let internal = internal_id.render();
        let name = format!("{internal}.class");
        let mut found = None;
        // Search only the entries the package tree says declare this class's package, in classpath order
        // (the spec's qualified-name step: search `node.jars`). The tree lists EVERY jar/dir/jimage that
        // declares the package, so the result is identical to scanning all entries — just fewer reads
        // (a probe for an absent class touches the one jar that owns the package, not every entry + the
        // jimage). Fall back to all entries when the package isn't cataloged (defensive).
        let pkg = internal.rsplit_once('/').map_or("", |(p, _)| p);
        let scoped = self
            .package_tree()
            .node_for(pkg)
            .filter(|n| !n.jars.is_empty())
            .map(|n| n.jars.clone());
        let indices = scoped.unwrap_or_else(|| (0..self.entries.len()).collect());
        for i in indices {
            let Some(e) = self.entries.get(i) else {
                continue;
            };
            let bytes = match e {
                Entry::Dir(d) => std::fs::read(d.join(&name)).ok(),
                Entry::Jar(j) => self.jar_entry(j, &name),
                // The JDK jimage stores classes uncompressed — seek-read the class via a one-time
                // name→(offset,size) index so JDK type members (String, collections, …) resolve.
                Entry::Jimage(_) => self.jimage_bytes(&internal),
            };
            if let Some(b) = bytes {
                if let Ok(ci) = parse_class(&b) {
                    // A DIRECTORY entry on a case-INSENSITIVE filesystem (macOS APFS) happily serves
                    // `java/lang/error.class` for `Error.class` — verify the parsed class IS the
                    // requested one (JVM names are case-sensitive; `error` must not resolve to `Error`).
                    if !ci.this_class_matches(&internal) {
                        continue;
                    }
                    found = Some(std::sync::Arc::new(ci));
                    break;
                }
            }
        }
        self.cache
            .classes
            .write()
            .unwrap()
            .insert(internal_id, found.clone());
        self.local_cache
            .borrow_mut()
            .insert(internal_id, found.clone());
        found
    }

    /// The raw `.class` bytes for an internal name (Kotlin built-in names mapped to JVM first), or
    /// `None` if absent. Unlike `find`, this keeps the bytes (the inline expander needs the body).
    fn class_bytes(&self, internal: &str) -> Option<Vec<u8>> {
        let internal = super::jvm_class_map::to_jvm_internal(internal);
        let name = format!("{internal}.class");
        for e in &self.entries {
            let bytes = match e {
                Entry::Dir(d) => std::fs::read(d.join(&name)).ok().filter(|b| {
                    // Case-insensitive-filesystem guard (see `find`): the served file must BE the
                    // requested class, not a case-collided sibling.
                    parse_class(b).is_ok_and(|ci| ci.this_class_matches(internal))
                }),
                Entry::Jar(j) => self.jar_entry(j, &name),
                Entry::Jimage(_) => self.jimage_bytes(internal),
            };
            if bytes.is_some() {
                return bytes;
            }
        }
        None
    }

    /// Lazily read (and cache) one method's bytecode body — the inline expander's entry point. Each
    /// `(class, method, descriptor)` body is read and parsed at most once, even across many call sites.
    pub fn method_code(&self, internal: &str, name: &str, descriptor: &str) -> Option<MethodCode> {
        let internal_id = type_name(internal);
        let key = (internal_id, name.to_string(), descriptor.to_string());
        if let Some(hit) = self.bodies.borrow_mut().get(&key) {
            cache_stat!(bodies, true);
            return hit.clone();
        }
        cache_stat!(bodies, false);
        let mut code = self
            .class_bytes(internal)
            .and_then(|b| read_method_code(&b, name, descriptor));
        if code.is_none() {
            // A multifile facade (`StandardKt`) has no method bodies — they live in its part classes,
            // which the facade *extends* (a superclass chain: `StandardKt` → `StandardKt__StandardKt`).
            let mut cur = self.find(internal).and_then(|ci| ci.super_class());
            while let Some(s) = cur {
                if s == "java/lang/Object" {
                    break;
                }
                if let Some(mc) = self
                    .class_bytes(&s)
                    .and_then(|b| read_method_code(&b, name, descriptor))
                {
                    code = Some(mc);
                    break;
                }
                cur = self.find(&s).and_then(|ci| ci.super_class());
            }
        }
        self.bodies.borrow_mut().insert(key, code.clone());
        code
    }

    /// Whether the selected JVM callable is `inline`, matching by `(jvm name, descriptor)` through the
    /// decoded Kotlin metadata. Use this once overload resolution has selected a concrete descriptor; it
    /// avoids a name-wide inline flag leaking from one overload to another.
    pub fn is_inline_callable_name(
        &self,
        internal: TypeName,
        name: &str,
        descriptor: &str,
        desc_params: &[Ty],
    ) -> bool {
        self.meta_functions_name(internal).iter().any(|f| {
            if !f.is_inline || f.jvm_name != name {
                return false;
            }
            if f.jvm_desc == Some(descriptor) {
                return true;
            }
            if f.jvm_desc.is_some() {
                return false;
            }
            let off = f.is_extension as usize;
            let end = off + f.value_params.len();
            end == desc_params.len()
                && f.value_params
                    .iter()
                    .zip(&desc_params[off..end])
                    .all(|(m, d)| meta_param_compat(m.ty, d))
        })
    }

    /// Whether `internal.name(...)` is a Kotlin `suspend` function, per the class's `@Metadata`
    /// `IS_SUSPEND` flag. A call to it is a coroutine suspension point. Includes the superclass walk
    /// needed for facade part classes.
    pub fn is_suspend_method_name(&self, internal: TypeName, name: &str) -> bool {
        let mut cur = Some(internal);
        while let Some(s) = cur.take() {
            if s.matches("java/lang/Object") {
                break;
            }
            if self.class_meta_name(s).suspend_names.contains(name) {
                return true;
            }
            match self.find_name(s) {
                Some(ci) => cur = ci.super_class,
                None => break,
            }
        }
        false
    }

    /// Find extension function candidates for `receiver_desc.method_name`.
    /// `receiver_desc` is a JVM type descriptor, e.g. `Ljava/lang/String;`.
    /// Returns all static methods in any classpath class whose first parameter matches.
    pub fn find_extensions(&self, receiver_desc: &str, method_name: &str) -> Vec<ExtCandidate> {
        self.ensure_ext_index();
        // A genuine top-level name is never reachable via a receiver.
        if self
            .ext
            .borrow()
            .as_ref()
            .is_some_and(|idx| idx.toplevel_only.contains(method_name))
        {
            return Vec::new();
        }
        // O(1): the rebuilt candidates are pre-grouped by receiver (first-parameter) descriptor.
        self.ext_by_name(method_name).render_by_recv(receiver_desc)
    }

    /// Every static method named `method_name` across the classpath (top-level functions and
    /// extensions), for resolving a receiver-less call. Includes non-public (`@InlineOnly`) candidates,
    /// each tagged via `ExtCandidate.public`; the caller filters — normal resolution is public-only.
    pub fn find_top_level(&self, method_name: &str) -> Vec<ExtCandidate> {
        self.ext_by_name(method_name).render_all()
    }

    /// The JVM descriptor of the static method named `jvm_name` on facade `root` (walking the multifile
    /// super chain) — the emit-handle fallback when a `@Metadata` function omits its `method_signature`.
    /// When `recv_desc` is `Some`, the method whose FIRST parameter (the extension receiver) matches it is
    /// chosen — a name like `maxOrNull` has many receiver-typed overloads (`[I`, `[D`, `Iterable`), so name
    /// alone would pick the wrong one; `None` takes the first method of that name.
    pub fn facade_method(
        &self,
        root: &str,
        jvm_name: &str,
        recv_desc: Option<&str>,
        ret_desc: Option<&str>,
        value_param_descs: Option<&[String]>,
    ) -> Option<ExtCandidate> {
        // The full expected parameter descriptor (receiver + value params), when both are known: it
        // disambiguates overloads that share the receiver AND return but differ by value param
        // (`appendLine(StringBuilder)` vs `appendLine(StringBuilder, int)`) — matching by receiver alone
        // silently collapses them to the first (no-arg) one.
        let want_params: Option<String> =
            value_param_descs.and_then(|vps| recv_desc.map(|rd| format!("{rd}{}", vps.concat())));
        // The parameter section of `c`'s descriptor (between the parens).
        let params_of = |c: &ExtCandidate| -> Option<String> {
            c.descriptor
                .split_once('(')
                .and_then(|(_, r)| r.split_once(')'))
                .map(|(p, _)| p.to_string())
        };
        let by_recv = |c: &ExtCandidate| match recv_desc {
            None => true,
            Some(rd) => {
                descriptor_parts(&c.descriptor)
                    .and_then(|(fp, _)| fp)
                    .as_deref()
                    == Some(rd)
            }
        };
        let named: Vec<ExtCandidate> = self
            .facade_statics(root)
            .into_iter()
            .filter(|c| c.name == jvm_name)
            .collect();
        // Prefer the FULL parameter match (receiver + value params) — it disambiguates same-receiver
        // overloads that differ by value param. Fall back to receiver-only when the full descriptor is not
        // known or matches nothing (e.g. a function-typed value param whose erased form isn't rebuilt here),
        // so a scope fn like `apply` still resolves to its real (`@InlineOnly`, private) method.
        let full: Vec<ExtCandidate> = match &want_params {
            Some(wp) => named
                .iter()
                .filter(|c| params_of(c).as_deref() == Some(wp.as_str()))
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        let cands: Vec<ExtCandidate> = if full.is_empty() {
            named.into_iter().filter(|c| by_recv(c)).collect()
        } else {
            full
        };
        let ret_of = |c: &ExtCandidate| c.descriptor.rsplit_once(')').map(|(_, r)| r.to_string());
        // A concrete expected return picks the exact overload (`maxOrNull(Iterable)Double`); a type-var
        // return (none given) prefers the generic-bound overload (`…Comparable`/`…Object`) over the numeric
        // specializations that share the receiver.
        match ret_desc {
            Some(rd) => cands
                .iter()
                .find(|c| ret_of(c).as_deref() == Some(rd))
                .cloned(),
            None => cands
                .iter()
                .find(|c| matches!(ret_of(c).as_deref(), Some("Ljava/lang/Comparable;")))
                .or_else(|| {
                    cands
                        .iter()
                        .find(|c| matches!(ret_of(c).as_deref(), Some("Ljava/lang/Object;")))
                })
                .cloned(),
        }
        .or_else(|| cands.into_iter().next())
    }

    /// Every static callable a facade `root` declares (all names), following the multifile-facade super
    /// chain — the name-agnostic form of [`Self::rebuild_ext_candidate_records`], used to build a package's
    /// [`Self::pkg_members`] in one pass. Each `ClassInfo` is served from the L1/L2 cache.
    fn facade_statics(&self, root: &str) -> Vec<ExtCandidate> {
        let mut out = Vec::new();
        let Some(root_ci) = self.find(root) else {
            return out;
        };
        let root_public = root_ci.is_public();
        let mut cur = Some(root.to_string());
        let mut visited = std::collections::HashSet::new();
        while let Some(cn) = cur {
            if !visited.insert(cn.clone()) {
                break;
            }
            let Some(ci) = self.find(&cn) else { break };
            for m in &ci.methods {
                if !m.is_static() || m.name.starts_with('<') {
                    continue;
                }
                if !root_public && m.is_public() {
                    continue;
                }
                let Some((_, ret_desc)) = descriptor_parts(&m.descriptor) else {
                    continue;
                };
                out.push(ExtCandidate {
                    owner: type_name(root),
                    name: m.name.clone(),
                    descriptor: m.descriptor.clone(),
                    ret_desc,
                    signature: m.signature.clone(),
                    public: root_public && m.is_public(),
                });
            }
            cur = ci.super_class();
        }
        out
    }

    /// The spec's `(jar, package) → PkgMembers`: the member index (`name → static callables`) that ONE
    /// jar/dir contributes for `pkg`, built once from that jar's `kotlin_module` facades and shared across
    /// classpaths (keyed by jar path + package in [`global_jar_pkg_members`]). Roots at the PUBLIC facade
    /// (`CollectionsKt`), not the package-private multifile PART (`CollectionsKt__…`) `kotlin_module`
    /// lists — the callable public statics live on the facade, and `facade_statics` drops a non-public
    /// root's public statics (the `@InlineOnly` rule).
    fn jar_pkg_members(&self, entry: &Entry, pkg: &str) -> JarPkgMembers {
        let key = (entry.path().to_path_buf(), pkg.to_string());
        let mut g = global_jar_pkg_members().lock().unwrap();
        if let Some(m) = g.get(&key) {
            return m.clone();
        }
        let jp = global_jar_packages().get_or_build(entry.path(), || build_jar_packages(entry));
        let mut m = PkgMembers::default();
        if let Some(pe) = jp.entry(pkg) {
            let mut seen_facade = std::collections::HashSet::new();
            for &part_id in &pe.facades {
                let part = jp.names.render(part_id);
                // The public multifile facade is the `__`-prefix of a part (`…/CollectionsKt__X` →
                // `…/CollectionsKt`); a single-file facade has no `__` and roots at itself.
                let facade = part.split_once("__").map_or(part.as_str(), |(f, _)| f);
                if !seen_facade.insert(facade.to_string()) {
                    continue;
                }
                let facade_id = m.owner_names.insert(facade);
                let metas = self.meta_functions(facade);
                for cand in self.facade_statics(facade) {
                    // Key each static by its @Metadata SOURCE name (`kotlin_name`), NOT its JVM name — a
                    // `@JvmName`-mangled extension (`sum` → `sumOfInt`) or value-class member resolves by
                    // the source name; the JVM name is emit-only, kept on the candidate. A static with no
                    // metadata (a Java method / synthetic) keeps its JVM name as the source key.
                    let source = metas
                        .iter()
                        .find(|m| m.jvm_name == cand.name)
                        .map(|m| m.kotlin_name.clone())
                        .unwrap_or_else(|| cand.name.clone());
                    // The receiver (first-parameter) descriptor marks `facade` as an extension owner for it
                    // — the scoped `find_extension_owners`. Recorded before `cand` is moved into the maps.
                    if let Some(recv) = descriptor_parts(&cand.descriptor).and_then(|(fp, _)| fp) {
                        let owners = m.owners_by_recv.entry(recv).or_default();
                        if owners.last().copied() != Some(facade_id) && !owners.contains(&facade_id)
                        {
                            owners.push(facade_id);
                        }
                    }
                    let jvm_name = cand.name.clone();
                    let idx = m.candidates.len();
                    m.candidates
                        .push(ExtCandidateRecord::from_candidate(facade_id, &cand));
                    m.by_jvm.entry(jvm_name).or_default().push(idx);
                    m.by_source.entry(source).or_default().push(idx);
                }
            }
        }
        let rc = std::sync::Arc::new(m);
        g.insert(key, rc.clone());
        rc
    }

    /// The PUBLIC multifile facades a package declares, from the `kotlin_module` catalog (the parts
    /// `…Kt__X` collapsed to their public facade `…Kt`), across every jar that declares the package. The
    /// `@Metadata`-driven extension/top-level discovery reads each facade's merged metadata — the source
    /// of truth — instead of scanning JVM statics. Declaration order, deduped.
    pub fn package_facades(&self, pkg: &str) -> Vec<TypeName> {
        let tree = self.package_tree();
        let mut out = Vec::new();
        let Some(node) = tree.node_for(pkg) else {
            return out;
        };
        for &jar_id in &node.jars {
            let Some(entry) = self.entries.get(jar_id) else {
                continue;
            };
            let jp = global_jar_packages().get_or_build(entry.path(), || build_jar_packages(entry));
            if let Some(pe) = jp.entry(pkg) {
                for &part_id in &pe.facades {
                    let part = jp.names.render(part_id);
                    let facade = part
                        .split_once("__")
                        .map_or_else(|| type_name_from(&jp.names, part_id), |(f, _)| type_name(f));
                    if !out.contains(&facade) {
                        out.push(facade);
                    }
                }
            }
        }
        out
    }

    /// The scoped, lazy analogue of [`Self::find_extensions`]: the [`ExtCandidate`]s named `jvm_name`
    /// (the bytecode method name) whose receiver (first-parameter) descriptor is `recv_desc`, declared by
    /// the `kotlin_module` facades of the in-scope `packages`. Tree-driven and cached per (jar, package) —
    /// NO whole-classpath `ensure_ext_index` scan. Only genuine Kotlin extensions live in facades, so the
    /// caller's `@Metadata` extension check still gates a top-level function whose first parameter happens
    /// to match. A package/facade is consulted at most once.
    pub fn extensions_in_scope(
        &self,
        recv_desc: &str,
        jvm_name: &str,
        packages: &[String],
    ) -> Vec<ExtCandidate> {
        let tree = self.package_tree();
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for pkg in packages {
            if !seen.insert(pkg.as_str()) {
                continue;
            }
            let Some(node) = tree.node_for(pkg) else {
                continue;
            };
            for &jar_id in &node.jars {
                let Some(entry) = self.entries.get(jar_id) else {
                    continue;
                };
                let members = self.jar_pkg_members(entry, pkg);
                if let Some(indices) = members.by_jvm.get(jvm_name) {
                    for &idx in indices {
                        let Some(c) = members.candidates.get(idx) else {
                            continue;
                        };
                        if descriptor_parts(&c.descriptor)
                            .and_then(|(fp, _)| fp)
                            .as_deref()
                            == Some(recv_desc)
                        {
                            out.push(c.render(&members.owner_names));
                        }
                    }
                }
            }
        }
        out
    }

    /// The scoped, lazy analogue of [`Self::find_extension_owners`]: the facades that declare a static
    /// whose receiver (first-parameter) descriptor is `recv_desc`, among the in-scope `packages`. Reads
    /// the per-(jar, package) `owners_by_recv` index — no `ensure_ext_index`.
    pub fn extension_owners_in_scope(&self, recv_desc: &str, packages: &[String]) -> Vec<TypeName> {
        let tree = self.package_tree();
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut seen_owner = std::collections::HashSet::new();
        for pkg in packages {
            if !seen.insert(pkg.as_str()) {
                continue;
            }
            let Some(node) = tree.node_for(pkg) else {
                continue;
            };
            for &jar_id in &node.jars {
                let Some(entry) = self.entries.get(jar_id) else {
                    continue;
                };
                let members = self.jar_pkg_members(entry, pkg);
                if let Some(owners) = members.owners_by_recv.get(recv_desc) {
                    for &id in owners {
                        let o = type_name_from(&members.owner_names, id);
                        if seen_owner.insert(o) {
                            out.push(o);
                        }
                    }
                }
            }
        }
        out
    }

    /// [`Self::find_extensions`] when `scope` is `None` (the whole-classpath eager index), else the
    /// scoped, lazy tree lookup [`Self::extensions_in_scope`]. The seam that lets one enrichment body draw
    /// its extension candidates from either backend.
    pub fn find_extensions_scoped(
        &self,
        recv_desc: &str,
        jvm_name: &str,
        scope: Option<&[String]>,
    ) -> Vec<ExtCandidate> {
        match scope {
            None => self.find_extensions(recv_desc, jvm_name),
            Some(pkgs) => self.extensions_in_scope(recv_desc, jvm_name, pkgs),
        }
    }

    /// [`Self::find_extension_owners`] when `scope` is `None`, else the scoped
    /// [`Self::extension_owners_in_scope`].
    pub fn find_extension_owners_scoped(
        &self,
        recv_desc: &str,
        scope: Option<&[String]>,
    ) -> Vec<TypeName> {
        match scope {
            None => self.find_extension_owners(recv_desc),
            Some(pkgs) => self.extension_owners_in_scope(recv_desc, pkgs),
        }
    }

    /// The static callables named `name` declared by the `kotlin_module` facades of the in-scope
    /// `packages` — the tree-driven, scope-pruned function/property lookup (spec § Functions). For each
    /// package it consults only the jars that declare it (the tree), composing their per-(jar, package)
    /// `PkgMembers`; NOT a whole-classpath scan. A package is consulted at most once.
    pub fn functions_in_scope(&self, name: &str, packages: &[String]) -> Vec<ExtCandidate> {
        let tree = self.package_tree();
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for pkg in packages {
            if !seen.insert(pkg.as_str()) {
                continue;
            }
            let Some(node) = tree.node_for(pkg) else {
                continue;
            };
            for &jar_id in &node.jars {
                let Some(entry) = self.entries.get(jar_id) else {
                    continue;
                };
                let members = self.jar_pkg_members(entry, pkg);
                if let Some(indices) = members.by_source.get(name) {
                    out.extend(members.render_indices(indices));
                }
            }
        }
        out
    }

    /// The spec's top-level memo lookup: the already-composed [`ResolvedSymbols`](crate::libraries::ResolvedSymbols)
    /// namespace record for a fully-qualified name, or `None` on a cold miss. The classpath `SymbolSource`
    /// composes the record once (classifier + callables) via `resolve_symbols` and stores it with
    /// [`memoize_symbols`](Self::memoize_symbols); every later resolution of the same fqn reuses it.
    pub fn cached_symbols(
        &self,
        fqn: &str,
    ) -> Option<std::rc::Rc<crate::libraries::ResolvedSymbols>> {
        self.cached_symbols_name(type_name(fqn))
    }

    pub fn cached_symbols_name(
        &self,
        fqn: TypeName,
    ) -> Option<std::rc::Rc<crate::libraries::ResolvedSymbols>> {
        self.symbols_memo.borrow_mut().get(&fqn).cloned()
    }

    /// Store the composed namespace record for `fqn` in the top-level memo, returning the shared `Rc` the
    /// caller hands back. See [`cached_symbols`](Self::cached_symbols).
    pub fn memoize_symbols(
        &self,
        fqn: &str,
        symbols: crate::libraries::ResolvedSymbols,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        self.memoize_symbols_name(type_name(fqn), symbols)
    }

    pub fn memoize_symbols_name(
        &self,
        fqn: TypeName,
        symbols: crate::libraries::ResolvedSymbols,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        let rc = std::rc::Rc::new(symbols);
        self.symbols_memo.borrow_mut().insert(fqn, rc.clone());
        rc
    }

    /// The memoized rebuild for `method_name`, shared across threads and grouped by receiver — a hot name
    /// (`map`, `let`) is walked once for the whole process, and both `find_top_level` and `find_extensions`
    /// are then O(1) reads. This is what makes the lazy index perform: the WHERE map is tiny + retained;
    /// candidate records are rebuilt on first use and kept only for queried names.
    fn ext_by_name(&self, method_name: &str) -> std::sync::Arc<ExtByName> {
        // L1: per-thread, no lock — the hot resolver path.
        if let Some(hit) = self.ext_l1.borrow_mut().get(method_name).cloned() {
            cache_stat!(ext_l1, true);
            return hit;
        }
        cache_stat!(ext_l1, false);
        // L2: process-global, shared across threads (the rebuild happens once here).
        if let Some(hit) = self
            .ext_candidates
            .read()
            .unwrap()
            .get(method_name)
            .cloned()
        {
            cache_stat!(ext_l2, true);
            self.ext_l1
                .borrow_mut()
                .insert(method_name.to_string(), hit.clone());
            return hit;
        }
        cache_stat!(ext_l2, false);
        self.ensure_ext_index();
        // Clone the small owner list, releasing the `ext` borrow before `rebuild` (which borrows the class
        // caches). Candidates are rebuilt from each owner's cached `ClassInfo`, then grouped by receiver.
        let ext = self.ext.borrow().as_ref().cloned();
        let roots = ext
            .as_ref()
            .and_then(|idx| idx.by_name.get(method_name).cloned())
            .unwrap_or_default();
        let mut grouped = ExtByName::default();
        if let Some(idx) = ext {
            for root_id in roots {
                let root = idx.owner_names.render(root_id);
                let owner = grouped.owner_names.insert_from(&idx.owner_names, root_id);
                for cand in self.rebuild_ext_candidate_records(owner, &root, method_name) {
                    let cand_idx = grouped.all.len();
                    if let Some(recv) = descriptor_parts(&cand.descriptor).and_then(|(fp, _)| fp) {
                        grouped.by_recv.entry(recv).or_default().push(cand_idx);
                    }
                    grouped.all.push(cand);
                }
            }
        }
        let rc = std::sync::Arc::new(grouped);
        self.ext_candidates
            .write()
            .unwrap()
            .insert(method_name.to_string(), rc.clone());
        self.ext_l1
            .borrow_mut()
            .insert(method_name.to_string(), rc.clone());
        rc
    }

    fn ensure_ext_index(&self) {
        if self.ext.borrow().is_some() {
            return;
        }
        let key: Vec<PathBuf> = self
            .entries
            .iter()
            .map(|e| e.path().to_path_buf())
            .collect();
        if let Some(idx) = global_ext_cache().lock().unwrap().get(&key) {
            *self.ext.borrow_mut() = Some(idx.clone());
            return;
        }
        // Compose the index from PER-ENTRY contributions: each jar/dir is scanned once (cached by its
        // path via `global_entry_ext`) and shared by every classpath that includes it, so a cp that only
        // adds one library reuses the stdlib's cached scan instead of rescanning it (rescanning the whole
        // stdlib per unique cp was ~30% of compile). Only the cross-entry `toplevel_only` decision — a
        // name is top-level EVERYWHERE and never an extension — is global, so it is computed here, and the
        // receiver keying (skip a `toplevel_only` name) is applied while merging.
        let parts: Vec<std::sync::Arc<EntryExt>> = self
            .entries
            .iter()
            .map(|e| global_entry_ext().get_or_build(e.path(), || build_entry_ext(e)))
            .collect();
        let mut top: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut ext_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for p in &parts {
            top.extend(p.toplevel_names.iter().map(String::as_str));
            ext_names.extend(p.ext_names.iter().map(String::as_str));
        }
        let toplevel_only: std::collections::HashSet<String> = top
            .iter()
            .filter(|n| !ext_names.contains(*n))
            .map(|s| s.to_string())
            .collect();
        let mut idx = ExtIndex {
            toplevel_only,
            ..ExtIndex::default()
        };
        for p in &parts {
            for (name, owners) in &p.by_name {
                for &owner in owners {
                    push_name_from_dedup(
                        &mut idx.owner_names,
                        &mut idx.by_name,
                        name,
                        &p.owner_names,
                        owner,
                    );
                }
            }
            for (recv, statics) in &p.by_recv_raw {
                for (name, owner) in statics {
                    if !idx.toplevel_only.contains(name) {
                        push_name_from_dedup(
                            &mut idx.owner_names,
                            &mut idx.by_recv_owners,
                            recv,
                            &p.owner_names,
                            *owner,
                        );
                    }
                }
            }
        }
        let idx = std::sync::Arc::new(idx);
        global_ext_cache().lock().unwrap().insert(key, idx.clone());
        *self.ext.borrow_mut() = Some(idx);
    }
}

/// Scan ONE classpath entry into its [`EntryExt`] contribution: collect each class's lean record, then
/// index the statics reachable via each class's super-walk WITHIN this entry (a Kotlin multifile facade
/// and its `*___*Kt` part classes are compiled into the same jar, so the chain never crosses entries).
/// The `toplevel_only` filter is a whole-classpath decision, so it is deferred to the per-cp compose in
/// [`Classpath::ensure_ext_index`] — here every receiver-taking static is recorded raw in `by_recv_raw`.
fn build_entry_ext(entry: &Entry) -> EntryExt {
    let mut names = NameTree::default();
    let mut all: HashMap<NameId, ClassLite> = HashMap::new();
    match entry {
        Entry::Dir(d) => collect_dir(d, &mut names, &mut all),
        Entry::Jar(j) => collect_jar(j, &mut names, &mut all),
        // No Kotlin extensions live in the JDK.
        Entry::Jimage(_) => {}
    }
    let mut ext = EntryExt::default();
    for lite in all.values() {
        ext.toplevel_names
            .extend(lite.toplevel_names.iter().cloned());
        ext.ext_names.extend(lite.ext_names.iter().cloned());
    }
    for (&root, lite) in &all {
        let mut root_id = None;
        let mut cur = Some(root);
        let mut visited = std::collections::HashSet::new();
        while let Some(cn) = cur {
            if !visited.insert(cn) {
                break;
            }
            let Some(c) = all.get(&cn) else { break };
            for (mname, mdesc, _msig, public) in &c.statics {
                if !lite.is_public && *public {
                    continue;
                }
                let Some((first_param, _ret_desc)) = descriptor_parts(mdesc) else {
                    continue;
                };
                let owner = match root_id {
                    Some(id) => id,
                    None => {
                        let id = ext.owner_names.insert_from(&names, root);
                        root_id = Some(id);
                        id
                    }
                };
                push_id_dedup(&mut ext.by_name, mname, owner);
                if let Some(recv) = first_param {
                    ext.by_recv_raw
                        .entry(recv)
                        .or_default()
                        .push((mname.clone(), owner));
                }
            }
            cur = c.super_class;
        }
    }
    ext
}

/// A classpath entry's index into `Classpath::entries` — the jar/dir a package or class comes from,
/// used only to order `find`/facade lookups by classpath declaration order.
type JarId = usize;

/// One package's facts within a single jar/dir. Built from the central-directory name pass plus the
/// jar's `kotlin_module`; the members (facade statics, builtins) are parsed lazily elsewhere. The fields
/// are the payload the later rollout steps consume (lazy facade/builtin resolution) — populated and
/// unit-tested now, read once resolution is routed through the tree.
#[allow(dead_code)]
#[derive(Default)]
struct PkgEntry {
    /// File-facade internal names declared for this package by `kotlin_module` (`kotlin/collections/
    /// CollectionsKt`). The roots whose `@Metadata` statics are the package's top-level/extension functions.
    facades: Vec<NameId>,
    /// The package directory holds `<pkg>/*.class` entries (regular classes / facades live here).
    has_classes: bool,
    /// The package has a `.kotlin_builtins` fragment (a builtin type with no `.class`: List, Int, Map…).
    has_builtins: bool,
}

/// One classpath entry's package catalog: which packages it declares, and per-package facts. Built once
/// per jar/dir (cached via [`EntryCache`]) from ONE shallow `kotlin_module` parse plus a
/// central-directory package-name pass (entry names only — no decompression, no class parse).
#[derive(Default)]
struct JarPackages {
    names: NameTree,
    /// slashed package name ID (`kotlin/collections`, `""` for the default package) → its facts.
    packages: HashMap<NameId, PkgEntry>,
}

impl JarPackages {
    fn entry(&self, pkg: &str) -> Option<&PkgEntry> {
        self.names.get(pkg).and_then(|id| self.packages.get(&id))
    }

    fn entry_mut(&mut self, pkg: &str) -> &mut PkgEntry {
        let id = self.names.insert(pkg);
        self.packages.entry(id).or_default()
    }
}

/// A node in the composed classpath package table: every jar that declares THIS package (union across the
/// classpath, in declaration order). One jar sits in many package nodes.
#[derive(Default)]
pub struct PackageNode {
    jars: Vec<JarId>,
}

#[derive(Default)]
pub struct PackageTree {
    names: NameTree,
    packages: HashMap<NameId, PackageNode>,
}

impl PackageTree {
    /// The node for a slashed package path (`""` = this root), or `None` if no jar declares it. The
    /// resolution seam (wired in a later rollout step); exercised now by the compose unit tests.
    #[allow(dead_code)]
    fn node_for(&self, pkg: &str) -> Option<&PackageNode> {
        self.names.get(pkg).and_then(|id| self.packages.get(&id))
    }

    /// Total package count in the table. For memory reporting.
    fn package_count(&self) -> usize {
        self.packages.len()
    }
}

/// Record one central-directory entry name into its package's facts (no bytes read). `a/b/C.class` marks
/// package `a/b` as having classes; `a/b/b.kotlin_builtins` marks it as having builtins.
fn record_pkg_entry_name(name: &str, jp: &mut JarPackages) {
    let pkg_of = |n: &str| {
        n.rsplit_once('/')
            .map_or(String::new(), |(p, _)| p.to_string())
    };
    if name.ends_with(".class") {
        jp.entry_mut(&pkg_of(name)).has_classes = true;
    } else if name.ends_with(".kotlin_builtins") {
        jp.entry_mut(&pkg_of(name)).has_builtins = true;
    }
}

/// Merge a jar's `kotlin_module` bytes into its catalog: each package's facade internal names.
fn record_kotlin_module(bytes: &[u8], jp: &mut JarPackages) {
    for (pkg, facades) in super::metadata::read_kotlin_module(bytes) {
        let pkg_id = jp.names.insert(&pkg);
        let facades = facades
            .iter()
            .map(|facade| jp.names.insert(facade))
            .collect::<Vec<_>>();
        jp.packages
            .entry(pkg_id)
            .or_default()
            .facades
            .extend(facades);
    }
}

/// Build one entry's [`JarPackages`] — the only eager per-jar work: a central-directory name pass plus
/// the shallow `kotlin_module` read(s). The JDK jimage contributes its package membership from the
/// location table (names only — no class parse), so `find` can scope a JDK type to the jimage instead
/// of scanning every entry (spec § jimage: "build package membership from its location table").
fn build_jar_packages(entry: &Entry) -> JarPackages {
    let mut jp = JarPackages::default();
    match entry {
        Entry::Jar(j) => build_jar_packages_jar(j, &mut jp),
        Entry::Dir(d) => build_jar_packages_dir(d, d, &mut jp),
        Entry::Jimage(p) => {
            if let Some(idx) = build_jimage_index(p) {
                for &internal in idx.by_name.keys() {
                    let Some(pkg) = idx.names.parent(internal) else {
                        continue;
                    };
                    if pkg == NameTree::ROOT {
                        continue;
                    }
                    let pkg = jp.names.insert_from(&idx.names, pkg);
                    jp.packages.entry(pkg).or_default().has_classes = true;
                }
            }
        }
    }
    jp
}

fn build_jar_packages_jar(jar: &Path, jp: &mut JarPackages) {
    let Ok(f) = File::open(jar) else { return };
    let Ok(mut archive) = zip::ZipArchive::new(f) else {
        return;
    };
    // Name pass over the central directory — no decompression. Defer reading `kotlin_module` bytes.
    let mut module_indices = Vec::new();
    for i in 0..archive.len() {
        let Ok(e) = archive.by_index(i) else { continue };
        let name = e.name();
        if name.starts_with("META-INF/") && name.ends_with(".kotlin_module") {
            module_indices.push(i);
        } else {
            record_pkg_entry_name(name, jp);
        }
    }
    for i in module_indices {
        let Ok(mut e) = archive.by_index(i) else {
            continue;
        };
        let mut buf = Vec::new();
        if e.read_to_end(&mut buf).is_ok() {
            record_kotlin_module(&buf, jp);
        }
    }
}

fn build_jar_packages_dir(root: &Path, dir: &Path, jp: &mut JarPackages) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            build_jar_packages_dir(root, &p, jp);
            continue;
        }
        let Ok(rel) = p.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.ends_with(".kotlin_module") {
            if let Ok(b) = std::fs::read(&p) {
                record_kotlin_module(&b, jp);
            }
        } else {
            record_pkg_entry_name(&rel, jp);
        }
    }
}

/// Compose per-jar [`JarPackages`] into the merged [`PackageTree`] — a cheap union: every package a jar
/// declares adds that jar to the package's node (in classpath declaration order).
fn compose_package_tree(parts: &[std::sync::Arc<JarPackages>]) -> PackageTree {
    let mut tree = PackageTree::default();
    for (jar_id, jp) in parts.iter().enumerate() {
        for &pkg_id in jp.packages.keys() {
            let pkg = tree.names.insert_from(&jp.names, pkg_id);
            let node = tree.packages.entry(pkg).or_default();
            if !node.jars.contains(&jar_id) {
                node.jars.push(jar_id);
            }
        }
    }
    tree
}

/// The classpath is the JVM realization of the inliner's narrow [`MethodBodies`] capability — the
/// emitter sees only this, not the whole `Classpath`.
impl super::inline::MethodBodies for Classpath {
    fn body(&self, owner: &str, name: &str, descriptor: &str) -> Option<MethodCode> {
        self.method_code(owner, name, descriptor)
    }
    fn owner_is_interface(&self, owner: &str) -> bool {
        self.find(owner)
            .map(|ci| ci.is_interface())
            .or_else(|| crate::jvm::jvm_class_map::jvm_mapped_builtin_is_interface(owner))
            .unwrap_or(false)
    }
}

/// A lean per-class record for building the extension index — only what's needed to follow facade
/// superclass chains and index static methods (no fields, no instance methods).
struct ClassLite {
    is_public: bool,
    super_class: Option<NameId>,
    /// `(name, descriptor, generic-signature, is_public)` of each static method (excl `<init>`/`<clinit>`).
    /// Non-public ones (`@InlineOnly`) are kept for the inliner; the flag gates normal resolution.
    statics: Vec<(String, String, Option<String>, bool)>,
    /// JVM names of functions `@Metadata` marks as genuine TOP-LEVEL (NO extension receiver). A top-level
    /// generic whose first parameter erases to `Object` (`assertEquals<T>(T, T, String)`) is otherwise
    /// indistinguishable in bytecode from an extension, so a name that is ONLY ever top-level must NOT be
    /// keyed by its first parameter in `by_recv`. Name-keyed (not name+desc): `@Metadata` often omits the
    /// method descriptor (`jvm_desc=None`).
    toplevel_names: std::collections::HashSet<String>,
    /// JVM names `@Metadata` marks as EXTENSIONS (receiver of any kind — class OR type parameter). A name
    /// that is an extension anywhere is NEVER excluded from `by_recv` (so `takeIf`/`uppercase` stay indexed).
    ext_names: std::collections::HashSet<String>,
}

fn collect_class_bytes(bytes: &[u8], names: &mut NameTree, all: &mut HashMap<NameId, ClassLite>) {
    let Ok(ci) = parse_class(bytes) else { return };
    let this_class = names.insert(&ci.this_class());
    let super_class = ci.super_class().map(|s| names.insert(&s));
    let statics = ci
        .methods
        .iter()
        .filter(|m| m.is_static() && !m.name.starts_with('<'))
        .map(|m| {
            (
                m.name.clone(),
                m.descriptor.clone(),
                m.signature.clone(),
                m.is_public(),
            )
        })
        .collect();
    // `@Metadata`-declared functions of this facade/part, split by whether they have an extension receiver
    // (of any kind — class or type parameter). Lets the ext index keep a genuine top-level generic out of
    // `by_recv` (its first JVM param looks like a receiver) without excluding a real extension.
    let mut toplevel_names = std::collections::HashSet::new();
    let mut ext_names = std::collections::HashSet::new();
    for mf in super::metadata::package_functions(&ci)
        .into_iter()
        .chain(super::metadata::class_functions(&ci))
    {
        if mf.is_extension {
            ext_names.insert(mf.jvm_name);
        } else {
            toplevel_names.insert(mf.jvm_name);
        }
    }
    all.insert(
        this_class,
        ClassLite {
            is_public: ci.is_public(),
            super_class,
            statics,
            toplevel_names,
            ext_names,
        },
    );
}

fn collect_dir(dir: &Path, names: &mut NameTree, all: &mut HashMap<NameId, ClassLite>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_dir(&p, names, all);
        } else if p.extension().map_or(false, |x| x == "class") {
            if let Ok(b) = std::fs::read(&p) {
                collect_class_bytes(&b, names, all);
            }
        }
    }
}

fn collect_jar(jar: &Path, names: &mut NameTree, all: &mut HashMap<NameId, ClassLite>) {
    let Ok(f) = File::open(jar) else { return };
    let Ok(mut archive) = zip::ZipArchive::new(f) else {
        return;
    };
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name();
        if !name.ends_with(".class") {
            continue;
        }
        // Kotlin top-level / extension functions are compiled to FILE-FACADE classes (`<File>Kt`) and
        // their package-private multifile PART classes (`<Facade>__<Part>`, also `…Kt…`) — kotlinc's
        // naming convention. The ext index only needs those, so skip every other class WITHOUT reading
        // it (a regular class / JDK type holds no resolvable top-level statics here). This avoids
        // parsing the thousands of non-facade stdlib classes — the dominant cost of building the index.
        let simple = name.rsplit('/').next().unwrap_or(name);
        if !simple.contains("Kt") {
            continue;
        }
        let mut buf = Vec::new();
        if entry.read_to_end(&mut buf).is_ok() {
            collect_class_bytes(&buf, names, all);
        }
    }
}

fn descriptor_parts(desc: &str) -> Option<(Option<String>, String)> {
    let params = desc.strip_prefix('(')?;
    let ret = params.find(')')?;
    let first = (!params.starts_with(')')).then(|| {
        let mut cursor = params;
        read_one_type(&mut cursor).to_string()
    });
    Some((first, params[ret + 1..].to_string()))
}

/// Read one complete JVM type descriptor from the start of `s`, advancing past it.
fn read_one_type<'a>(s: &mut &'a str) -> &'a str {
    let orig = *s;
    match s.chars().next() {
        Some('[') => {
            *s = &s[1..];
            read_one_type(s); // element
            let consumed = orig.len() - s.len();
            &orig[..consumed]
        }
        Some('L') => {
            let end = s.find(';').map(|i| i + 1).unwrap_or(s.len());
            let t = &s[..end];
            *s = &s[end..];
            t
        }
        Some(_) => {
            let t = &s[..1];
            *s = &s[1..];
            t
        }
        None => "",
    }
}

/// `Xxx.class` entry name (jar/jimage path) → internal name, or `None` if not an indexable class.
fn class_internal_from_entry(name: &str) -> Option<&str> {
    name.strip_suffix(".class").filter(|s| !s.is_empty())
}

/// Parse Kotlin type aliases from a file facade's `@Metadata` (the `Package.typeAlias` proto entries).
/// A top-level `typealias` lands in its file facade (`Lib.kt` → `LibKt`), not only the stdlib's
/// dedicated `*TypeAliasesKt` files, so every `*Kt` facade is parsed — the proto reader only emits real
/// alias entries (unlike the old `d2` `$annotations` heuristic, which a facade's annotated top-level
/// property would have tripped).
fn parse_aliases_from_bytes(bytes: &[u8], idx: &mut TypeIndex) {
    let Ok(ci) = parse_class(bytes) else { return };
    for (alias, internal) in super::metadata::package_type_aliases(&ci) {
        idx.type_aliases
            .insert(type_name(&alias), type_name(&internal));
    }
}

/// A Kotlin FILE FACADE (`*Kt`) — where a top-level `typealias` is recorded. Parsed for aliases; every
/// other class is indexed by name alone. (`TypeAliasesKt` is just the stdlib's conventional facade name;
/// a general library's alias lives in its own `<File>Kt` facade.)
fn is_type_aliases_kt(internal: &str) -> bool {
    internal
        .rsplit('/')
        .next()
        .unwrap_or(internal)
        .ends_with("Kt")
}

/// Build ONE classpath entry's type-alias table — the per-entry unit `EntryCache` memoizes (built once
/// per jar, race-free). The JDK jimage carries no Kotlin metadata, so it contributes nothing.
fn build_entry_types(entry: &Entry) -> TypeIndex {
    let mut idx = TypeIndex::default();
    match entry {
        Entry::Dir(d) => scan_types_dir(d, &mut idx),
        Entry::Jar(j) => scan_types_jar(j, &mut idx),
        Entry::Jimage(_) => {}
    }
    idx
}

fn scan_types_dir(dir: &Path, idx: &mut TypeIndex) {
    scan_types_dir_rooted(dir, dir, idx);
}

/// Walk `dir` for `*TypeAliasesKt.class` files and decode their Kotlin type aliases. Other classes are
/// skipped — the classpath no longer builds a name → internal map (it was dead; import-driven resolution
/// goes through `resolve_type` / the ext index).
fn scan_types_dir_rooted(root: &Path, dir: &Path, idx: &mut TypeIndex) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            scan_types_dir_rooted(root, &p, idx);
        } else if p.extension().map_or(false, |x| x == "class") {
            let Ok(rel) = p.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            let Some(internal) = class_internal_from_entry(&rel) else {
                continue;
            };
            if is_type_aliases_kt(internal) {
                if let Ok(b) = std::fs::read(&p) {
                    parse_aliases_from_bytes(&b, idx);
                }
            }
        }
    }
}

fn scan_types_jar(jar: &Path, idx: &mut TypeIndex) {
    let Ok(f) = File::open(jar) else { return };
    let Ok(mut archive) = zip::ZipArchive::new(f) else {
        return;
    };
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name().to_string();
        let Some(internal) = class_internal_from_entry(&name) else {
            continue;
        };
        // Parse bytes only for the rare alias-carrier classes — everything else is skipped.
        if is_type_aliases_kt(internal) {
            let mut buf = Vec::new();
            if entry.read_to_end(&mut buf).is_ok() {
                parse_aliases_from_bytes(&buf, idx);
            }
        }
    }
}

/// Build the jimage class index: internal name id → [`JimageEntry`] (content offset + on-disk size +
/// compressed flag) for each `.class` resource, read from the jimage location table directly — the
/// bootclasspath equivalent of a jar's central directory — so JDK class bytes can be seek-read on demand.
/// Format reference (little-endian header): jdk.internal.jimage.BasicImageReader / ImageHeader /
/// ImageLocation.
fn build_jimage_index(path: &Path) -> Option<JimageIndex> {
    use std::io::Read;
    // Read ONLY the header + location/string tables (a few MB), NOT the ~146 MB content blob that follows
    // — the index just stores each resource's content OFFSET; the bytes are seek-read on demand
    // (`jimage_bytes`). Reading the whole image was a ~146 MB peak-RSS spike per worker thread.
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 28];
    f.read_exact(&mut head).ok()?;
    let h =
        |o: usize| u32::from_le_bytes([head[o], head[o + 1], head[o + 2], head[o + 3]]) as usize;
    if h(0) != 0xCAFE_DADA {
        return None;
    }
    let table_length = h(16);
    let locations_size = h(20);
    let strings_size = h(24);
    let header = 28;
    let offsets = header + table_length * 4;
    let locations = offsets + table_length * 4;
    let strings = locations + locations_size;
    let content = strings + strings_size;
    let mut b = vec![0u8; content];
    use std::io::Seek;
    f.rewind().ok()?;
    f.read_exact(&mut b).ok()?;
    let u32le = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let read_str = |off: usize| -> &str {
        if off == 0 {
            return "";
        }
        let s = strings + off;
        let mut e = s;
        while e < b.len() && b[e] != 0 {
            e += 1;
        }
        std::str::from_utf8(&b[s..e]).unwrap_or("")
    };
    // Decode an ImageLocation into attributes by kind: 2=PARENT, 3=BASE, 4=EXTENSION, 5=OFFSET,
    // 6=COMPRESSED, 7=UNCOMPRESSED.
    let decode = |mut p: usize| -> [usize; 8] {
        let mut a = [0usize; 8];
        while p < b.len() {
            let byte = b[p];
            p += 1;
            let kind = (byte >> 3) as usize;
            if kind == 0 {
                break;
            }
            let len = ((byte & 0x7) + 1) as usize;
            let mut v = 0usize;
            for _ in 0..len {
                if p >= b.len() {
                    break;
                }
                v = (v << 8) | b[p] as usize;
                p += 1;
            }
            if kind < 8 {
                a[kind] = v;
            }
        }
        a
    };
    let mut idx = JimageIndex::default();
    for i in 0..table_length {
        let lo = u32le(offsets + i * 4) as usize;
        if lo == 0 {
            continue;
        }
        let a = decode(locations + lo);
        if read_str(a[4]) != "class" {
            continue;
        }
        let parent = read_str(a[2]);
        if parent.is_empty() {
            continue;
        }
        let internal = format!("{parent}/{}", read_str(a[3]));
        let (off, comp, unc) = (a[5], a[6], a[7]);
        let abs = content + off;
        // Store the ON-DISK byte count: the compressed size for a compressed resource (a JetBrains
        // Runtime / `jlink --compress` image), else the uncompressed size. `compressed` (comp != 0) comes
        // from the location table alone — the `CompressedResourceHeader` magic check that CONFIRMS the
        // "zip" scheme is deferred to `jimage_bytes` (which reads the content anyway), so the index build
        // needs only the tables, not the content.
        let stored = if comp != 0 { comp } else { unc };
        let internal = idx.names.insert(&internal);
        idx.by_name
            .entry(internal)
            .or_insert((abs as u64, stored, comp != 0));
    }
    Some(idx)
}

#[cfg(test)]
mod fq_tests {
    use super::*;

    #[test]
    fn name_tree_shares_segments_and_renders_internal_names() {
        let mut tree = NameTree::default();
        let collections = tree.insert("kotlin/collections/CollectionsKt");
        let maps = tree.insert("kotlin/collections/MapsKt");
        let duplicate = tree.insert("kotlin/collections/CollectionsKt");

        assert_eq!(collections, duplicate);
        assert_eq!(tree.render(collections), "kotlin/collections/CollectionsKt");
        assert_eq!(tree.render(maps), "kotlin/collections/MapsKt");
        assert_eq!(tree.len(), 5);
    }

    #[test]
    fn name_tree_copies_between_indexes_without_render_dedup() {
        let mut entry_names = NameTree::default();
        let collections = entry_names.insert("kotlin/collections/CollectionsKt");
        let maps = entry_names.insert("kotlin/collections/MapsKt");

        let mut index_names = NameTree::default();
        let mut owners = HashMap::new();
        push_name_from_dedup(
            &mut index_names,
            &mut owners,
            "map",
            &entry_names,
            collections,
        );
        push_name_from_dedup(
            &mut index_names,
            &mut owners,
            "map",
            &entry_names,
            collections,
        );
        push_name_from_dedup(&mut index_names, &mut owners, "map", &entry_names, maps);

        let copied = owners.get("map").expect("owner ids copied");
        assert_eq!(copied.len(), 2);
        assert_eq!(
            index_names.render(copied[0]),
            "kotlin/collections/CollectionsKt"
        );
        assert_eq!(index_names.render(copied[1]), "kotlin/collections/MapsKt");
        assert_eq!(index_names.len(), 5);
    }

    #[test]
    fn jimage_index_uses_name_ids_for_class_lookup_and_package_parent() {
        let mut idx = JimageIndex::default();
        let string = idx.names.insert("java/lang/String");
        idx.by_name.insert(string, (1, 2, false));

        let lookup = idx.names.get("java/lang/String").expect("indexed class");
        assert_eq!(idx.by_name.get(&lookup), Some(&(1, 2, false)));

        let package = idx.names.parent(string).expect("class has package parent");
        let mut packages = JarPackages::default();
        let package = packages.names.insert_from(&idx.names, package);
        packages.packages.entry(package).or_default().has_classes = true;

        assert_eq!(packages.names.render(package), "java/lang");
        assert!(packages.packages[&package].has_classes);
    }

    #[test]
    fn class_cache_uses_type_names_for_l1_l2_keys() {
        let cache = ClassCacheData::default();
        let first = type_name("kotlin/collections/List");
        let second = type_name("kotlin/collections/List");
        let map = type_name("kotlin/collections/Map");

        assert_eq!(first, second);
        assert_ne!(first, map);
        cache.classes.write().unwrap().insert(first, None);
        assert!(cache.classes.read().unwrap().contains_key(&second));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn ext_by_name_cache_records_owner_ids_until_render() {
        let mut cached = ExtByName::default();
        let owner = cached
            .owner_names
            .insert("kotlin/collections/CollectionsKt");
        let record = ExtCandidateRecord {
            owner,
            name: "map".to_string(),
            descriptor: "(Ljava/lang/Iterable;)Ljava/util/List;".to_string(),
            ret_desc: "Ljava/util/List;".to_string(),
            signature: None,
            public: true,
        };

        cached.all.push(record);
        cached
            .by_recv
            .entry("Ljava/lang/Iterable;".to_string())
            .or_default()
            .push(0);

        assert_eq!(cached.owner_names.len(), 4);
        let all = cached.render_all();
        assert!(all[0].owner.matches("kotlin/collections/CollectionsKt"));
        let by_recv = cached.render_by_recv("Ljava/lang/Iterable;");
        assert!(by_recv[0].owner.matches("kotlin/collections/CollectionsKt"));
    }

    #[test]
    fn package_member_cache_indexes_id_backed_candidates() {
        let mut members = PkgMembers::default();
        let owner = members
            .owner_names
            .insert("kotlin/collections/CollectionsKt");
        members.candidates.push(ExtCandidateRecord {
            owner,
            name: "sumOfInt".to_string(),
            descriptor: "(Ljava/lang/Iterable;)I".to_string(),
            ret_desc: "I".to_string(),
            signature: None,
            public: true,
        });
        members
            .by_source
            .entry("sumOf".to_string())
            .or_default()
            .push(0);
        members
            .by_jvm
            .entry("sumOfInt".to_string())
            .or_default()
            .push(0);

        assert_eq!(members.owner_names.len(), 4);
        assert_eq!(members.by_source["sumOf"], vec![0]);
        assert_eq!(members.by_jvm["sumOfInt"], vec![0]);
        let rendered = members.render_indices(&members.by_source["sumOf"]);
        assert!(rendered[0]
            .owner
            .matches("kotlin/collections/CollectionsKt"));
        assert_eq!(rendered[0].name, "sumOfInt");
    }

    #[test]
    fn type_index_composes_alias_targets_as_type_names() {
        let mut part = TypeIndex::default();
        let array_list_alias = type_name("kotlin/collections/ArrayList");
        let array_list = type_name("java/util/ArrayList");
        part.type_aliases.insert(array_list_alias, array_list);

        let mut idx = TypeIndex::default();
        for (&alias, &target) in &part.type_aliases {
            idx.type_aliases.entry(alias).or_insert(target);
        }

        let target = idx.type_aliases[&array_list_alias];
        assert!(target.matches("java/util/ArrayList"));
        assert!(!idx.is_empty());
    }

    #[test]
    fn metadata_param_matching_keeps_unsigned_descriptor_erasure() {
        let uint = type_name("kotlin/UInt");
        let ulong = type_name("kotlin/ULong");
        assert!(meta_param_compat(Some(uint), &Ty::Int));
        assert!(meta_param_compat(Some(ulong), &Ty::Long));
        assert!(meta_param_exact(Some(uint), &Ty::Int));
        assert!(meta_param_exact(Some(ulong), &Ty::Long));

        assert!(!meta_param_compat(Some(uint), &Ty::Long));
        assert!(!meta_param_compat(Some(ulong), &Ty::Int));
        assert!(!meta_param_exact(Some(uint), &Ty::Long));
        assert!(!meta_param_exact(Some(ulong), &Ty::Int));
    }

    /// The provisioned kotlin-stdlib jar via the project's single CI-safe resolver
    /// ([`crate::toolchain::stdlib_jar`] — the dist env vars `KRUSTY_KOTLINC`/`KRUSTY_KOTLIN_STDLIB`, then
    /// the gradle/m2 caches). A test returns early when it is absent (toolchain not provisioned), so it
    /// never fails on CI regardless of where the stdlib lives.
    fn test_stdlib_jar() -> Option<PathBuf> {
        crate::toolchain::stdlib_jar()
    }

    // Every `Classpath` gets a distinct process-unique `id`, EVEN when an earlier instance has been
    // dropped (and its heap address could be reused). Per-classpath caches (the library seed) key on this
    // id, so a freed-then-reallocated `Classpath` cannot collide with a stale entry — the regression that
    // made a cross-module class go unresolved on the *second* compile in a process (the first compile's
    // seed, missing that module, was served via a reused `Rc<Classpath>` pointer address).
    #[test]
    fn classpath_ids_are_unique_across_realloc() {
        let id_a = {
            let a = Classpath::new(vec![PathBuf::from("/nonexistent/a")]);
            a.id()
        }; // `a` dropped here — its address is now free to be reused by `b`.
        let b = Classpath::new(vec![PathBuf::from("/nonexistent/b")]);
        assert_ne!(id_a, b.id(), "a reallocated Classpath must not reuse an id");
        let c = Classpath::new(vec![PathBuf::from("/nonexistent/c")]);
        assert_ne!(b.id(), c.id(), "distinct live Classpaths have distinct ids");
    }

    fn jar_packages(pkgs: &[(&str, PkgEntry)]) -> std::sync::Arc<JarPackages> {
        let mut jp = JarPackages::default();
        for (p, e) in pkgs {
            let entry = jp.entry_mut(p);
            entry.has_classes = e.has_classes;
            entry.has_builtins = e.has_builtins;
        }
        std::sync::Arc::new(jp)
    }

    #[test]
    fn compose_unions_jars_per_package_and_nests() {
        let jar0 = jar_packages(&[
            (
                "kotlin/collections",
                PkgEntry {
                    has_classes: true,
                    ..PkgEntry::default()
                },
            ),
            (
                "kotlin",
                PkgEntry {
                    has_classes: true,
                    ..PkgEntry::default()
                },
            ),
        ]);
        // A second jar ALSO declares `kotlin/collections` — the node must list both jars, in cp order.
        let jar1 = jar_packages(&[(
            "kotlin/collections",
            PkgEntry {
                has_builtins: true,
                ..PkgEntry::default()
            },
        )]);
        let tree = compose_package_tree(&[jar0, jar1]);
        assert!(tree.names.get("kotlin/collections").is_some());
        assert_eq!(tree.node_for("kotlin").unwrap().jars, vec![0]);
        assert_eq!(
            tree.node_for("kotlin/collections").unwrap().jars,
            vec![0, 1]
        );
        assert!(tree.node_for("kotlin/ranges").is_none());
        // `kotlin` and `kotlin/collections` are the two packages.
        assert_eq!(tree.package_count(), 2);
    }

    #[test]
    fn record_entry_name_classifies_packages() {
        let mut jp = JarPackages::default();
        record_pkg_entry_name("kotlin/collections/CollectionsKt.class", &mut jp);
        record_pkg_entry_name("kotlin/collections/collections.kotlin_builtins", &mut jp);
        record_pkg_entry_name("Top.class", &mut jp); // default package
        let c = jp.entry("kotlin/collections").unwrap();
        assert!(c.has_classes && c.has_builtins);
        assert!(jp.entry("").unwrap().has_classes);
    }

    #[test]
    fn jar_package_catalog_stores_package_and_facade_ids() {
        let mut jp = JarPackages::default();
        let pkg = jp.names.insert("kotlin/collections");
        let facade = jp.names.insert("kotlin/collections/CollectionsKt");
        jp.packages.entry(pkg).or_default().facades.push(facade);

        let entry = jp.entry("kotlin/collections").unwrap();
        assert_eq!(entry.facades, vec![facade]);
        assert_eq!(
            jp.names.render(entry.facades[0]),
            "kotlin/collections/CollectionsKt"
        );
        assert!(jp.entry("kotlin/text").is_none());
    }

    #[test]
    fn real_stdlib_jar_declares_known_packages_and_facades() {
        let Some(jar) = test_stdlib_jar() else {
            return;
        };
        let jp = build_jar_packages(&Entry::Jar(jar));
        // Central-directory pass sees the class-bearing + builtins packages.
        let coll = jp.entry("kotlin/collections").unwrap();
        assert!(coll.has_classes, "kotlin/collections has .class entries");
        assert!(
            coll.has_builtins,
            "kotlin/collections has a .kotlin_builtins"
        );
        // `kotlin_module` names the multifile-facade PART classes (`CollectionsKt__CollectionsKt`),
        // which carry the package's top-level statics — exactly the roots lazy facade parsing needs.
        assert!(
            coll.facades.iter().any(|&f| jp
                .names
                .render(f)
                .starts_with("kotlin/collections/CollectionsKt")),
            "kotlin_module names the CollectionsKt parts, got {:?}",
            coll.facades
                .iter()
                .map(|&f| jp.names.render(f))
                .collect::<Vec<_>>()
        );
        // Compose into a tree; the nested package resolves and the root does not falsely appear.
        let tree = compose_package_tree(&[std::sync::Arc::new(jp)]);
        assert_eq!(tree.node_for("kotlin/collections").unwrap().jars, vec![0]);
        assert!(tree.node_for("kotlin").unwrap().jars == vec![0]);
    }

    #[test]
    fn tree_routed_find_matches_a_real_stdlib_class_and_misses_absent() {
        let Some(jar) = test_stdlib_jar() else {
            return;
        };
        let cp = Classpath::new(vec![jar]);
        // A real facade part in kotlin/collections resolves through the package-scoped entry search.
        assert!(
            cp.find("kotlin/collections/CollectionsKt").is_some(),
            "scoped find must locate a class in a cataloged package"
        );
        // An absent class in a REAL package resolves to None (the negative probe, now scoped to the one
        // jar that owns the package) — and is cached.
        assert!(cp.find("kotlin/collections/DoesNotExistXyz").is_none());
        // An absent class in a package no jar declares also misses (falls back cleanly).
        assert!(cp.find("no/such/pkg/Nope").is_none());
    }

    #[test]
    fn resolve_symbols_records_function_and_classifier_namespaces() {
        use crate::libraries::Callables;
        use crate::symbol_source::SymbolSource;
        let Some(jar) = test_stdlib_jar() else {
            return;
        };
        let cp = std::rc::Rc::new(Classpath::new(vec![jar]));
        let libs = crate::jvm::jvm_libraries::JvmLibraries::new(cp.clone());
        // A top-level function occupies the CALLABLE namespace, not the classifier one — found via the
        // package's `kotlin_module` facades (tree-driven, no whole-classpath scan).
        let f = libs.resolve_symbols("kotlin/collections/emptyList");
        assert!(f.classifier.is_none(), "emptyList is not a classifier");
        assert!(
            matches!(f.callables, Callables::Functions(_)),
            "emptyList is a classpath callable"
        );
        // A class occupies the CLASSIFIER namespace (first-jar-wins internal name).
        let c = libs.resolve_symbols("kotlin/Pair");
        assert!(c.classifier.is_some(), "Pair is a classifier");
        // An unknown name is absent in both namespaces.
        assert!(libs
            .resolve_symbols("kotlin/collections/definitelyNotAThingXyz")
            .is_empty());
        // Memoized (LRU): the same fqn returns the same `Rc` from the classpath's top-level memo.
        libs.resolve_symbols("kotlin/Pair");
        let a = cp.cached_symbols("kotlin/Pair").expect("memoized");
        let b = cp.cached_symbols("kotlin/Pair").expect("memoized");
        assert!(std::rc::Rc::ptr_eq(&a, &b));
    }

    #[test]
    fn functions_in_scope_is_tree_pruned() {
        let Some(jar) = test_stdlib_jar() else {
            return;
        };
        let cp = Classpath::new(vec![jar]);
        // In scope: emptyList (kotlin/collections) resolves via the tree-driven per-package lookup.
        let coll = vec!["kotlin/collections".to_string()];
        assert!(cp
            .functions_in_scope("emptyList", &coll)
            .iter()
            .any(|c| c.name == "emptyList"));
        // Out of scope: the same name does NOT resolve (kotlinc import visibility) — the lookup only
        // consults the given packages' facades, never the whole classpath.
        let text = vec!["kotlin/text".to_string()];
        assert!(cp.functions_in_scope("emptyList", &text).is_empty());
    }

    #[test]
    fn extensions_in_scope_matches_scope_filtered_eager_index() {
        let Some(jar) = test_stdlib_jar() else {
            return;
        };
        let cp = Classpath::new(vec![jar]);
        let coll = vec!["kotlin/collections".to_string()];
        let recv = "Ljava/lang/Iterable;";
        // The scoped, tree-driven enumeration returns exactly the eager index's candidates whose owner
        // facade sits in the scoped package — the equivalence the `select_overload` switch relies on.
        let want_owner_in_scope = |c: &ExtCandidate| c.owner.package_matches("kotlin/collections");
        let mut eager: Vec<_> = cp
            .find_extensions(recv, "map")
            .into_iter()
            .filter(want_owner_in_scope)
            .map(|c| (c.owner.render(), c.name, c.descriptor))
            .collect();
        let mut lazy: Vec<_> = cp
            .extensions_in_scope(recv, "map", &coll)
            .into_iter()
            .map(|c| (c.owner.render(), c.name, c.descriptor))
            .collect();
        eager.sort();
        lazy.sort();
        assert!(!lazy.is_empty(), "map is an Iterable extension in scope");
        assert_eq!(lazy, eager, "tree-scoped == scope-filtered eager index");
        // Owner query agrees on the PUBLIC facade: the eager index records the multifile PART
        // (`…Kt__…`), the tree the `__`-stripped public facade (`…Kt`) — the form `meta_functions` and
        // the emit path use. Compare facade-normalized.
        let facade_of = |o: &str| o.split_once("__").map_or(o, |(f, _)| f).to_string();
        let owners = cp.extension_owners_in_scope(recv, &coll);
        assert!(
            owners.iter().all(|o| o.starts_with("kotlin/collections/")),
            "scoped owners live in the scoped package"
        );
        assert!(
            cp.find_extension_owners(recv)
                .iter()
                .filter(|o| o.starts_with("kotlin/collections/"))
                .all(|o| owners.contains(&type_name(&facade_of(&o.render())))),
            "every in-scope eager owner's facade is a scoped owner"
        );
        // Out of scope: an Iterable extension is invisible when its package is not imported.
        assert!(cp
            .extensions_in_scope(recv, "map", &["kotlin/text".to_string()])
            .is_empty());
    }

    #[test]
    fn package_facades_lists_public_multifile_facades() {
        let Some(jar) = test_stdlib_jar() else {
            return;
        };
        let cp = Classpath::new(vec![jar]);
        let facades = cp.package_facades("kotlin/collections");
        // The public facade is listed (the `__`-part is collapsed to it) and deduped.
        assert!(facades
            .iter()
            .any(|f| f.matches("kotlin/collections/CollectionsKt")));
        assert!(
            !facades.iter().any(|f| f.contains("__")),
            "parts collapse to the public facade"
        );
        let deduped: HashSet<_> = facades.iter().copied().collect();
        assert_eq!(deduped.len(), facades.len(), "no duplicate facades");
        // A package no jar declares yields nothing.
        assert!(cp.package_facades("no/such/pkg").is_empty());
    }

    #[test]
    fn facade_method_descriptor_disambiguates_by_receiver_and_return() {
        let Some(jar) = test_stdlib_jar() else {
            return;
        };
        let cp = Classpath::new(vec![jar]);
        let facade = "kotlin/collections/CollectionsKt";
        // `maxOrNull` has many same-named receiver overloads; the receiver descriptor selects the
        // Iterable form, and a concrete return descriptor the numeric specialization.
        let d = cp.facade_method(
            facade,
            "maxOrNull",
            Some("Ljava/lang/Iterable;"),
            Some("Ljava/lang/Double;"),
            None,
        );
        assert_eq!(
            d.map(|c| c.descriptor).as_deref(),
            Some("(Ljava/lang/Iterable;)Ljava/lang/Double;")
        );
        // A type-variable return (None) prefers the generic-bound (`Comparable`) overload.
        let g = cp.facade_method(
            facade,
            "maxOrNull",
            Some("Ljava/lang/Iterable;"),
            None,
            None,
        );
        assert_eq!(
            g.map(|c| c.descriptor).as_deref(),
            Some("(Ljava/lang/Iterable;)Ljava/lang/Comparable;")
        );
        // A name with no method on the facade chain is absent.
        assert!(cp
            .facade_method(facade, "definitelyNotAMethodXyz", None, None, None)
            .is_none());
    }

    #[test]
    fn resolve_symbols_returns_classifier_and_callable_namespaces() {
        let Some(jar) = test_stdlib_jar() else {
            return;
        };
        use crate::libraries::Callables;
        use crate::symbol_source::SymbolSource;
        let lib =
            crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![
                jar,
            ])));
        // Classifier namespace: a class fqn resolves its classifier, no callables.
        let c = lib.resolve_symbols("kotlin/Pair");
        assert!(c.classifier.is_some(), "kotlin/Pair is a classifier");
        assert!(matches!(c.callables, Callables::None));
        // Callable namespace: a top-level function fqn resolves callables, no classifier.
        let f = lib.resolve_symbols("kotlin/collections/emptyList");
        assert!(f.classifier.is_none());
        assert!(
            matches!(&f.callables, Callables::Functions(s) if !s.overloads.is_empty()),
            "emptyList resolves as callables"
        );
        // Extension namespace: `map` is a kotlin/collections extension — resolve_symbols surfaces it as
        // a receiver-agnostic Extension callable (discovered source-keyed via the tree).
        let m = lib.resolve_symbols("kotlin/collections/map");
        assert!(
            matches!(&m.callables, Callables::Functions(s)
                if s.overloads.iter().any(|o| o.kind == crate::libraries::FnKind::Extension)),
            "map resolves as an extension callable"
        );
    }

    #[test]
    fn builtin_member_misses_are_cached() {
        let cp = Classpath::new(vec![PathBuf::from("/nonexistent/stdlib.jar")]);
        assert!(cp.builtin_members.borrow().is_empty());
        assert!(cp.builtin_members("kotlin/String").is_empty());
        let string = type_name("kotlin/String");
        assert!(cp.builtin_members.borrow().contains_key(&string));
        assert!(cp.builtin_members("kotlin/String").is_empty());
    }
}
