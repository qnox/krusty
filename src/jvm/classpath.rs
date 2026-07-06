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
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::jvm::classreader::{parse_class, read_method_code, ClassInfo, MethodCode};
use crate::jvm::names::type_descriptor;
use crate::libraries::{CallSig, FunctionSet, ReturnInfo};
use crate::types::Ty;

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

/// Map a `@Metadata` SOURCE value-parameter type internal name to a `Ty` for call matching. A function
/// type (`kotlin/Function0`) stays a semantic function type; a type-parameter param (`None`) erases to
/// `kotlin/Any` (accepts any arg).
fn meta_param_ty(name: Option<&str>) -> Ty {
    let Some(n) = name else {
        return Ty::obj("kotlin/Any");
    };
    if let Some(arity) = n.strip_prefix("kotlin/Function") {
        if !arity.is_empty() && arity.bytes().all(|b| b.is_ascii_digit()) {
            let n = arity.parse::<usize>().unwrap_or(0);
            return Ty::fun(vec![Ty::obj("kotlin/Any"); n], Ty::obj("kotlin/Any"));
        }
    }
    // Core carries `Array<T>` (`Ty::Array`), never the JVM-metadata array spellings (`kotlin/IntArray`,
    // `kotlin/Array`). A primitive-array class fixes its element (`kotlin/IntArray` → `Array<Int>`); a
    // generic `Array<T>` erases its element here (the class name alone doesn't carry it — arrays align
    // element-agnostically). This keeps the resolver/checker platform-neutral.
    if n == "kotlin/Array" {
        return Ty::array(Ty::obj("kotlin/Any"));
    }
    if let Some(elem) = n
        .strip_prefix("kotlin/")
        .and_then(|s| s.strip_suffix("Array"))
    {
        let et = kotlin_name_to_ty(&format!("kotlin/{elem}"));
        if !matches!(et, Ty::Obj(..)) {
            return Ty::array(et);
        }
    }
    kotlin_name_to_ty(n)
}

/// Whether `meta` (a `@Metadata` source value param `Ty`) aligns with `desc` (a JVM-descriptor param
/// `Ty`) when matching an overload structurally. Exact when equal; a generic/erased param (`kotlin/Any`
/// or its erased `java/lang/Object`) on either side matches any reference type; otherwise the class
/// names must match (so a `Function0` source param anchors to a `Function0` descriptor param and won't
/// be confused with an `Any`/`Object` one).
fn ty_compat(meta: &Ty, desc: &Ty) -> bool {
    // A metadata type keeps its Kotlin name (`kotlin/collections/Iterable`, `kotlin/IntArray`, a type
    // parameter's bound); the descriptor carries the mapped JVM type (`java/lang/Iterable`, `[I`, the
    // erased bound). They denote the SAME JVM parameter when their erased descriptors are equal — computed
    // through the same mapped-types machinery the emitter uses, so no per-type casing is needed here.
    if crate::jvm::names::type_descriptor(*meta) == crate::jvm::names::type_descriptor(*desc) {
        return true;
    }
    // A type-parameter / `Any` metadata type erases to `Object`, which accepts any reference (a generic
    // param spelled `T` records as `kotlin/Any`, so this is how it matches a concrete descriptor position).
    // Conversely, when the DESCRIPTOR is `Object` (a type variable, or a value class whose underlying is
    // `Any`) a concrete metadata object CLASS matches it — but NOT an array or a function type (`Ty::Array`
    // / `Ty::Fun`), which never erase to a plain `Object` descriptor. Both are LOOSE matches: the caller
    // prefers an overload whose params match EXACTLY (equal descriptors), so `plusAssign(element: T)` /
    // `plusAssign(elements: Iterable)` bind their own descriptors rather than one swallowing both.
    let erased =
        |t: &Ty| matches!(t, Ty::Obj(n, _) if *n == "kotlin/Any" || *n == "java/lang/Object");
    if (erased(meta) && desc.is_reference()) || (erased(desc) && matches!(meta, Ty::Obj(_, _))) {
        return true;
    }
    // A `vararg`/array metadata param erases its element (`Array<T>` → `Array<Any>`), while the descriptor
    // has the concrete array (`[Lkotlin/Pair;`) — match array against array element-agnostically so a
    // vararg overload aligns rather than losing to an empty sibling.
    matches!(meta, Ty::Array(_)) && matches!(desc, Ty::Array(_))
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
    functions: CacheCounter,
    meta_fns: CacheCounter,
    bodies: CacheCounter,
    suspend_names: CacheCounter,
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
            "class L1 {} · L2 {} | ext L1 {} · L2 {} | fns {} | meta_fns {} | bodies {} | \
             suspend {} | builtin {}",
            s.l1_class.line("hits"),
            s.l2_class.line("hits"),
            s.ext_l1.line("hits"),
            s.ext_l2.line("hits"),
            s.functions.line("hits"),
            s.meta_fns.line("hits"),
            s.bodies.line("hits"),
            s.suspend_names.line("hits"),
            s.builtin_members.line("hits"),
        );
    }
}

/// One jimage resource: `(file offset, ON-DISK byte size, zlib-compressed?)`. The size is the stored
/// (compressed) length when the resource uses the "zip" decompressor, else the raw class length; the
/// flag is set ONLY for the "zip" decompressor (authoritatively, from the strings table) so the reader
/// never inflates a resource compressed by some other scheme.
type JimageEntry = (u64, usize, bool);
type JimageIndex = HashMap<String, JimageEntry>;

/// Process-global jimage index (name → file offset/size), keyed by the jimage path. The jimage is
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

/// The rebuilt candidates for ONE method name, grouped for O(1) receiver lookup so `find_extensions`
/// doesn't re-scan + re-parse the whole list on every call site (the cost the eager `by_recv` map avoided).
#[derive(Default)]
struct ExtByName {
    /// first-parameter descriptor (the extension receiver) → candidates.
    by_recv: HashMap<String, Vec<ExtCandidate>>,
    /// every candidate of this name (top-level + extensions), for the receiver-less `find_top_level`.
    all: Vec<ExtCandidate>,
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

/// Process-global cache of parsed `ClassInfo` (internal name → parsed class, `None` if absent), keyed
/// by the classpath. The conformance harness compiles on several rayon worker threads, EACH with its
/// own `Classpath`; without sharing, every common class (`kotlin/collections/List`, …) was parsed once
/// per thread. Sharing this — like the type/ext/jimage indexes — parses each class once per process.
/// `RwLock` because reads (cache hits) dominate; a parse on a miss takes the write lock briefly.
type ClassCache =
    std::sync::Arc<std::sync::RwLock<HashMap<String, Option<std::sync::Arc<ClassInfo>>>>>;
fn global_class_cache(key: &[PathBuf]) -> ClassCache {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<Vec<PathBuf>, ClassCache>>> =
        std::sync::OnceLock::new();
    let m = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut g = m.lock().unwrap();
    g.entry(key.to_vec())
        .or_insert_with(|| std::sync::Arc::new(std::sync::RwLock::new(HashMap::new())))
        .clone()
}

/// One resolved extension-function candidate: the owner class (internal name), the JVM method
/// descriptor, the method name, and the return-type descriptor.
#[derive(Clone, Debug)]
pub struct ExtCandidate {
    pub owner: String,
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
/// the L1/L2 caches already hold), via [`Classpath::rebuild_ext_candidates`]. This keeps ~a few MB of
/// name/owner strings resident instead of materializing every stdlib static twice (the old eager index
/// was the single largest retained allocation — ~195 MB — per heap profiling).
#[derive(Default)]
struct ExtIndex {
    /// method name → facade/part ROOT class names whose super-walk declares a static of that name.
    by_name: HashMap<String, Vec<String>>,
    /// receiver descriptor → the owner facades that declare an extension on it (for `find_extension_owners`).
    by_recv_owners: HashMap<String, Vec<String>>,
    /// Names `@Metadata` marks as GENUINE top-level (a receiver-less function, never an extension) — these
    /// are never keyed by their first parameter, so `find_extensions` must not return them for any receiver.
    toplevel_only: std::collections::HashSet<String>,
}

/// Classpath Kotlin type aliases (`typealias X = Y` in a library), simple alias name → JVM internal name.
/// A simple/FQ name → internal CLASS map used to live here too, but name resolution is import-driven (via
/// `resolve_type` probes and the ext index's `resolve_top_level_callable`), not table-driven — verified by
/// building it empty with no test regression. Building it eagerly for every class on the classpath (the
/// whole ~30k-class JDK jimage included) was ~85 MB of retained dead weight + a full-image name scan.
#[derive(Default, Clone, Debug)]
pub struct TypeIndex {
    /// Kotlin type alias simple name → JVM internal name
    /// (e.g. `"StringBuilder"` → `"java/lang/StringBuilder"`).
    pub type_aliases: HashMap<String, String>,
}

/// Per-class `@Metadata` cache: class internal name → every function decoded from its `Package` metadata
/// (with the multifile-facade part classes merged in). This is the SINGLE decode of a class's `d1` for the
/// function lookups below — `meta_functions`, `metadata_receiver_types`, `metadata_call_facts`, and
/// parameter metadata all project over it instead of each re-decoding and re-merging.
type MetaFnsCache = RefCell<crate::lru::LruCache<String, std::rc::Rc<ClassMeta>>>;

/// One top-level callable decoded from `@Metadata`, with the fields classpath lookup needs kept
/// together so return type, receiver, nullability, parameter names/defaults, and receiver-lambda
/// annotations cannot drift across parallel maps.
struct MetaCallable {
    kotlin_name: String,
    jvm_name: String,
    receiver_class: Option<&'static str>,
    is_extension: bool,
    ret: ReturnInfo,
    value_params: Vec<MetaCallableParam>,
}

struct MetaCallableParam {
    ty: Ty,
    name: String,
    has_default: bool,
    materialized: bool,
    recv_fun: bool,
    recv_fun_receiver: Option<String>,
}

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
/// (facade parts merged). Computed once per class in [`Classpath::class_meta`]; the public
/// `metadata_*` methods just index these maps.
struct ClassMeta {
    callables: Vec<MetaCallable>,
    by_kotlin_name: HashMap<String, Vec<usize>>,
    by_jvm_name: HashMap<String, Vec<usize>>,
    /// The full facade-merged [`MetaFn`] list this is projected from — exposed via
    /// [`Classpath::meta_functions`] for the lookups that need a whole `MetaFn` (return class by JVM
    /// name, receiver-function params) rather than one of the maps above, so they share THIS decode
    /// instead of re-decoding + re-merging the `d1` themselves.
    fns: std::rc::Rc<[super::metadata::MetaFn]>,
}

/// Whether metadata callable `c` corresponds to a JVM method with these descriptor parameter types. An
/// EXTENSION's receiver — a separate attribute, emitted as the leading JVM parameter — must match, then
/// the value parameters align in order. Returns `(kept-param end, exact-match count)` — `end` is the count
/// of SOURCE parameters (where the synthetic tail — a `suspend` Continuation, a `$default` mask — begins),
/// and `exact` counts the value params matching by EQUAL erased descriptor (not through the loose
/// type-variable rule), so the caller prefers the most-specific overload (`plusAssign(element: T)` binds
/// the `Object` descriptor, `plusAssign(elements: Iterable)` the `Iterable` one).
fn meta_callable_aligns(c: &MetaCallable, desc_params: &[Ty]) -> Option<(usize, usize)> {
    use crate::jvm::names::type_descriptor;
    let off = c.is_extension as usize;
    let end = off + c.value_params.len();
    if end > desc_params.len() {
        return None;
    }
    let receiver_ok = !c.is_extension
        || match &c.receiver_class {
            Some(rc) => ty_compat(&kotlin_name_to_ty(rc), &desc_params[0]),
            None => desc_params[0].is_reference(),
        };
    if !receiver_ok
        || !c
            .value_params
            .iter()
            .zip(&desc_params[off..end])
            .all(|(m, d)| ty_compat(&m.ty, d))
    {
        return None;
    }
    let exact = c
        .value_params
        .iter()
        .zip(&desc_params[off..end])
        .filter(|(m, d)| type_descriptor(m.ty) == type_descriptor(**d))
        .count();
    Some((end, exact))
}

/// Pick the metadata function whose signature corresponds to the JVM method with `desc_params`, returning
/// `(kept-param end, index into `meta.callables`/`meta.fns`)`. Disambiguates OVERLOADS sharing a JVM name
/// (`any()` vs `any(predicate)`, `IntArray.any` vs `CharArray.any`) by receiver + value-parameter match,
/// preferring the longest alignment. Both `meta.callables` and `meta.fns` index by the same function list.
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
            let c = &meta.callables[i];
            let (end, exact) = meta_callable_aligns(c, desc_params)?;
            // Return match disambiguates overloads that differ ONLY by return (`sum` → `sumOfInt`/
            // `sumOfLong`, same erased params). Soft tiebreaker: a concrete metadata return equal to the
            // descriptor's return wins, but a generic/type-parameter return (`class` None) or one that
            // erases differently (a value class vs its underlying) is left to the params match, so a sole
            // candidate still wins.
            let ret_match = c.ret.class.is_some_and(|rc| ty_compat(&rc, desc_ret));
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
) -> Option<(usize, &'a MetaCallable)> {
    aligned_meta_index(meta, fn_name, desc_params, desc_ret)
        .map(|(end, i)| (end, &meta.callables[i]))
}

pub(super) fn metadata_return_info(class: Option<&str>, nullable: bool) -> ReturnInfo {
    ReturnInfo::new(nullable, class.map(kotlin_name_to_ty))
}

/// Per-class `@Metadata` cache: class internal name → Kotlin function names that participate in
/// `@OverloadResolutionByLambdaReturnType` (`sumOf`, …). The resolver derives and verifies the concrete
/// JVM method (`sumOfInt`/`sumOfLong`/…) from the lambda return type, so the cache only needs membership.
type LambdaReturnOverloads = std::collections::HashSet<String>;
type MetaOverloadCache = RefCell<crate::lru::LruCache<String, std::rc::Rc<LambdaReturnOverloads>>>;

#[derive(Default)]
pub struct Classpath {
    entries: Vec<Entry>,
    // Two-level parsed-class cache: `local` is a per-thread L1 (cheap `RefCell`, no lock — serves the
    // hot repeated lookups), backed by `shared` — a process-global L2 (`RwLock`) so a class is PARSED
    // once across all rayon worker threads, not once per thread. L1 miss → L2 → parse.
    local_cache: RefCell<crate::lru::LruCache<String, Option<std::sync::Arc<ClassInfo>>>>,
    cache: ClassCache,
    /// Open `ZipArchive` per jar path, so reading an entry is a central-directory hash lookup + inflate
    /// — NOT a re-parse of the whole central directory (which `zip::ZipArchive::new` does, thousands of
    /// entries for kotlin-stdlib). This is the classloader/javac strategy: parse each jar's directory
    /// once, then read class bytes lazily on demand. Profiling showed the per-read re-parse dominated
    /// type checking. Lives behind a `RefCell` (one `Classpath` per thread; never shared across threads).
    archives: RefCell<HashMap<PathBuf, zip::ZipArchive<File>>>,
    ext: RefCell<Option<std::sync::Arc<ExtIndex>>>,
    types: RefCell<Option<std::sync::Arc<TypeIndex>>>,
    /// Lazily-built index of the JDK jimage: internal class name → [`JimageEntry`], so JDK class bytes
    /// can be seek-read (and inflated, for a compressed image) on demand. Shared via `Arc` from a
    /// process-global cache so the 146 MB parse happens once.
    jimage: RefCell<Option<(PathBuf, std::sync::Arc<JimageIndex>)>>,
    /// Cache of lazily-read method bodies (`(internal, name, descriptor) → MethodCode`), so the inline
    /// expander reads each inline function's body once even when it's called many times.
    bodies: RefCell<crate::lru::LruCache<(String, String, String), Option<MethodCode>>>,
    /// Cache of the `suspend` function names declared by a class (from its `@Metadata` `IS_SUSPEND`
    /// flag), so suspension-point recognition at a call site doesn't re-decode the metadata per call.
    suspend_names:
        RefCell<crate::lru::LruCache<String, std::rc::Rc<std::collections::HashSet<String>>>>,
    /// Cache of each class's decoded `@Metadata` functions (facade parts merged) — the single decode the
    /// return-type / receiver / nullability / kept-param lookups all project over (see [`MetaFnsCache`]).
    meta_fns: MetaFnsCache,
    /// Cache of each class's `@Metadata` Kotlin-name → `@JvmName` overloads (see [`MetaOverloadCache`]).
    meta_overloads: MetaOverloadCache,
    /// Cache of resolved library function sets keyed by semantic call query. A `JvmLibraries` wrapper is
    /// rebuilt for every snippet, but the `Classpath` is reused on the worker thread, so keeping this here
    /// avoids re-walking metadata/extension indexes for common stdlib calls across thousands of snippets.
    functions: RefCell<crate::lru::LruCache<(String, Option<Ty>), FunctionSet>>,
    /// Cache of resolved `LibraryType`s by internal name. Like `functions`, kept on the reused-per-thread
    /// `Classpath` (NOT the per-compile `JvmLibraries`) so the import-driven `resolve_type` probing — which
    /// asks for the same stdlib types across thousands of snippets — warms across compiles instead of
    /// rebuilding each `LibraryType` (descriptor parses + `@Metadata` decodes) from cold every file.
    resolved_types:
        RefCell<crate::lru::LruCache<String, Option<std::rc::Rc<crate::libraries::LibraryType>>>>,
    /// Parsed `.kotlin_builtins` fragments, keyed by resource path (e.g. `kotlin/kotlin.kotlin_builtins`,
    /// `kotlin/collections/collections.kotlin_builtins`), each mapping class internal name → its
    /// supertypes + members. Built once per file on first use — the single source for BOTH the collection
    /// read-only/mutable hierarchy AND every builtin type's API. Empty if no stdlib is on the classpath.
    builtins: RefCell<HashMap<String, std::rc::Rc<HashMap<String, super::metadata::BuiltinClass>>>>,
    /// Resolved builtin member vectors, keyed by Kotlin internal class name. The raw builtins fragment is
    /// already cached, but mapping it to `LibraryMember`s also resolves JVM owners/interface flags and
    /// allocates descriptors. `resolve_type` asks for these repeatedly during member/subtype lookup.
    builtin_members:
        RefCell<crate::lru::LruCache<String, std::rc::Rc<Vec<crate::libraries::LibraryMember>>>>,
    /// Rebuilt ext/top-level candidates per method name (the lazy [`ExtIndex`]'s `by_name` gives WHERE;
    /// this memoizes the actual rebuilt records so a hot stdlib name isn't re-walked on every query). Two
    /// levels, like the parsed-class cache: `ext_l1` is a per-thread `RefCell` — a CHEAP borrow on the hot
    /// resolver path (`find_extensions` is called per call site) — holding `Arc`s shared from `ext_candidates`,
    /// the process-global L2 where the one-time rebuild lives. Both hold only QUERIED names (the working set).
    ext_l1: RefCell<crate::lru::LruCache<String, std::sync::Arc<ExtByName>>>,
    ext_candidates: ExtCandCache,
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
            jimage: RefCell::new(None),
            bodies: RefCell::new(crate::lru::LruCache::new(BODY_CAP)),
            suspend_names: RefCell::new(crate::lru::LruCache::new(META_CAP)),
            meta_fns: RefCell::new(crate::lru::LruCache::new(META_CAP)),
            meta_overloads: RefCell::new(crate::lru::LruCache::new(META_CAP)),
            functions: RefCell::new(crate::lru::LruCache::new(FN_CAP)),
            resolved_types: RefCell::new(crate::lru::LruCache::new(CLASS_CAP)),
            builtins: RefCell::new(HashMap::new()),
            builtin_members: RefCell::new(crate::lru::LruCache::new(META_CAP)),
            ext_l1: RefCell::new(crate::lru::LruCache::new(FN_CAP)),
            ext_candidates: global_ext_candidates(&cache_key),
            id,
        }
    }

    /// Process-unique identity assigned at construction — a stable cache key for per-classpath caches
    /// (see the `id` field). Unlike an `Rc<Classpath>` pointer, this never aliases a freed classpath.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// A one-line snapshot of every cache's entry count — for memory profiling (`KRUSTY_MEM_REPORT`). The
    /// per-`Classpath` caches (`L1_class`/`fns`/`meta*`/`bodies`/`builtin`) are LRU-bounded, so they
    /// plateau at their caps; the shared `L2_class` map and the `jimage`/`type`/`ext` INDEXES are the
    /// library-sized structures (the jimage names every JDK class) — the ones to watch if RSS is high.
    pub fn cache_report(&self) -> String {
        let jimage = self.jimage.borrow().as_ref().map_or(0, |(_, i)| i.len());
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
        format!(
            "classpath#{} L1_class={} L2_class={} fns={} meta_fns={} meta_ovl={} suspend={} bodies={} \
             builtin={} | jimage={} type={} ext={}",
            self.id,
            self.local_cache.borrow().len(),
            self.cache.read().unwrap().len(),
            self.functions.borrow().len(),
            self.meta_fns.borrow().len(),
            self.meta_overloads.borrow().len(),
            self.suspend_names.borrow().len(),
            self.bodies.borrow().len(),
            self.builtin_members.borrow().len(),
            jimage,
            types,
            ext,
        )
    }

    pub fn cached_functions(&self, key: &(String, Option<Ty>)) -> Option<FunctionSet> {
        let hit = self.functions.borrow_mut().get(key).cloned();
        cache_stat!(functions, hit.is_some());
        hit
    }

    pub fn cache_functions(&self, key: (String, Option<Ty>), set: FunctionSet) {
        self.functions.borrow_mut().insert(key, set);
    }

    /// Memoized `resolve_type` result for `internal` (the outer `Option` = cached-vs-not; the inner =
    /// resolved-vs-absent). Warm across compiles because this `Classpath` is reused per worker thread.
    pub fn cached_library_type(
        &self,
        internal: &str,
    ) -> Option<Option<std::rc::Rc<crate::libraries::LibraryType>>> {
        self.resolved_types.borrow_mut().get(internal).cloned()
    }

    pub fn cache_library_type(
        &self,
        internal: &str,
        ty: Option<std::rc::Rc<crate::libraries::LibraryType>>,
    ) {
        self.resolved_types
            .borrow_mut()
            .insert(internal.to_string(), ty);
    }

    /// The decoded `@Metadata` function lookups for `internal` (facade parts merged), decoded once and
    /// cached. The single `d1` decode that `meta_functions`/`metadata_receiver_types`/
    /// `metadata_call_facts` all project over.
    fn class_meta(&self, internal: &str) -> std::rc::Rc<ClassMeta> {
        if let Some(m) = self.meta_fns.borrow_mut().get(internal) {
            cache_stat!(meta_fns, true);
            return m.clone();
        }
        cache_stat!(meta_fns, false);
        let ci = self.find(internal);
        let mut fns = ci
            .as_ref()
            .map(|c| super::metadata::package_functions(c))
            .unwrap_or_default();
        // A multifile FACADE has no function metadata of its own — its `d1` lists the PART class names,
        // which hold the functions; merge them in (the parts' `d1` is decoded once here, not per lookup).
        if fns.is_empty() {
            if let Some(ci) = &ci {
                for part in &ci.kotlin_d1 {
                    if let Some(pci) = self.find(part) {
                        fns.extend(super::metadata::package_functions(&pci));
                    }
                }
            }
        }
        let callables: Vec<MetaCallable> = fns
            .iter()
            .map(|f| MetaCallable {
                kotlin_name: f.kotlin_name.clone(),
                jvm_name: f.jvm_name.clone(),
                receiver_class: f.receiver_class,
                is_extension: f.is_extension,
                ret: metadata_return_info(f.ret_class, f.ret_nullable),
                value_params: f
                    .value_params
                    .iter()
                    .map(|p| MetaCallableParam {
                        ty: meta_param_ty(p.ty.as_deref()),
                        name: p.name.clone(),
                        has_default: p.has_default,
                        materialized: p.materialized,
                        recv_fun: p.recv_fun,
                        recv_fun_receiver: p.recv_fun_receiver.clone(),
                    })
                    .collect(),
            })
            .collect();
        let mut by_kotlin_name: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_jvm_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, c) in callables.iter().enumerate() {
            by_kotlin_name
                .entry(c.kotlin_name.clone())
                .or_default()
                .push(i);
            by_jvm_name.entry(c.jvm_name.clone()).or_default().push(i);
        }
        let meta = std::rc::Rc::new(ClassMeta {
            callables,
            by_kotlin_name,
            by_jvm_name,
            fns: fns.into(),
        });
        self.meta_fns
            .borrow_mut()
            .insert(internal.to_string(), meta.clone());
        meta
    }

    /// Every `@Metadata` function of `internal` (a facade's PART classes merged), decoded once and
    /// cached — the single source the metadata-primary `MetaFn` lookups share. Use this instead of
    /// re-calling `package_functions` + re-merging the facade parts at each call site.
    pub fn meta_functions(&self, internal: &str) -> std::rc::Rc<[super::metadata::MetaFn]> {
        self.class_meta(internal).fns.clone()
    }

    /// Whether `@Metadata` describes a function named `jvm_name` on `internal` (facade parts merged). When
    /// it does, the metadata signature is authoritative — callers must not fall back to the JVM `Signature`.
    pub fn has_meta_function(&self, internal: &str, jvm_name: &str) -> bool {
        self.class_meta(internal).by_jvm_name.contains_key(jvm_name)
    }

    /// The metadata-primary [`GenericSig`] for the `internal.jvm_name` overload corresponding to the JVM
    /// method with `desc_params`. kotlinc omits the `method_signature` extension when it equals the
    /// computed default, so the correct overload is picked by aligning the metadata signature to the
    /// descriptor (receiver + value parameters) — the SAME selection the call-fact lookup uses, so both
    /// agree. `None` when `@Metadata` has no matching function (a Java method / synthetic).
    pub fn aligned_generic_sig(
        &self,
        internal: &str,
        jvm_name: &str,
        desc_params: &[Ty],
        desc_ret: &Ty,
    ) -> Option<crate::libraries::GenericSig> {
        let meta = self.class_meta(internal);
        let (_, idx) = aligned_meta_index(&meta, jvm_name, desc_params, desc_ret)?;
        meta.fns.get(idx).and_then(|f| f.generic_sig.clone())
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
        let meta = self.class_meta(internal);
        let Some((end, c)) = aligned_meta_callable(&meta, fn_name, desc_params, desc_ret) else {
            return MetadataCallFacts::fallback(if extension {
                CallSig::default()
            } else {
                CallSig::metadata_plain(desc_params.len())
            });
        };
        MetadataCallFacts {
            kept_params: Some(end),
            call_sig: if extension {
                CallSig::metadata_extension(
                    end,
                    c.value_params.iter().map(|p| p.name.clone()).collect(),
                    c.value_params.iter().map(|p| p.has_default).collect(),
                )
            } else {
                CallSig::metadata_top_level(
                    end,
                    c.value_params.iter().map(|p| p.name.clone()).collect(),
                    c.value_params.iter().map(|p| p.has_default).collect(),
                    c.value_params
                        .iter()
                        .map(|p| p.recv_fun_receiver.as_deref().map(Ty::obj))
                        .collect(),
                    c.value_params.iter().map(|p| p.recv_fun).collect(),
                    c.value_params.iter().map(|p| p.materialized).collect(),
                )
            },
            ret: c.ret,
        }
    }

    /// The source-level call and return facts of class MEMBER `internal.jvm_name/arity`, from the class's
    /// own `@Metadata` function record. Names, default flags, return classifier, and nullability come
    /// from the SAME member record, so a data-class `copy`, value-class-mangled member, or `suspend`
    /// return cannot drift across separate metadata lookups.
    pub fn metadata_member_call_facts(
        &self,
        internal: &str,
        jvm_name: &str,
        arity: usize,
    ) -> MetadataCallFacts {
        let Some(ci) = self.find(internal) else {
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
            call_sig: CallSig::metadata_member(
                arity,
                f.value_params.iter().map(|p| p.name.clone()).collect(),
                f.value_params.iter().map(|p| p.has_default).collect(),
            ),
            ret: metadata_return_info(f.ret_class, f.ret_nullable),
        }
    }

    /// All Kotlin extension-receiver internal names of `fn_name` in `internal` (`plusAssign` →
    /// `[kotlin/collections/MutableCollection, …/MutableMap]`), from `@Metadata`. A name is overloaded
    /// across receivers, so a receiver applies if it is a subtype of ANY entry. The JVM signature erases
    /// the receiver to its first parameter; only `@Metadata` keeps the read-only/mutable identity. Empty
    /// for a non-extension function.
    pub fn metadata_receiver_types(&self, internal: &str, fn_name: &str) -> Vec<String> {
        let meta = self.class_meta(internal);
        let mut out = Vec::new();
        if let Some(idxs) = meta.by_kotlin_name.get(fn_name) {
            for &i in idxs {
                if let Some(cn) = meta.callables[i].receiver_class {
                    let cn = cn.to_string();
                    if !out.contains(&cn) {
                        out.push(cn);
                    }
                }
            }
        }
        out
    }

    /// A facade class's lambda-return-overload Kotlin names, cached (part-merged for a multifile facade).
    pub fn lambda_return_overloads(&self, internal: &str) -> std::rc::Rc<LambdaReturnOverloads> {
        if let Some(m) = self.meta_overloads.borrow_mut().get(internal) {
            return m.clone();
        }
        // Overloads of one Kotlin name are split across the multifile facade's PART classes (the
        // `Int`/`Long`/`Double` `sumOf` in one part, `UInt`/`ULong` in another). The facade EXTENDS its
        // parts, so union every class's own metadata up the superclass chain — exactly how the extension
        // index reaches the part methods (a part isn't listed in the facade's `d1`).
        let mut names = LambdaReturnOverloads::new();
        let mut cur = Some(internal.to_string());
        let mut seen = std::collections::HashSet::new();
        while let Some(cn) = cur {
            if !seen.insert(cn.clone()) {
                break;
            }
            let Some(ci) = self.find(&cn) else { break };
            for f in self.meta_functions(&cn).iter() {
                if f.jvm_desc.is_some() && f.ret_class.is_some() {
                    names.insert(f.kotlin_name.clone());
                }
            }
            cur = ci.super_class.clone();
        }
        let rc = std::rc::Rc::new(names);
        self.meta_overloads
            .borrow_mut()
            .insert(internal.to_string(), rc.clone());
        rc
    }

    /// Every distinct owner (facade) that declares a static method whose first parameter matches
    /// `receiver_desc` — the facades to consult for a Kotlin-name resolution (`sumOf`).
    pub fn find_extension_owners(&self, receiver_desc: &str) -> Vec<String> {
        self.ensure_ext_index();
        self.ext
            .borrow()
            .as_ref()
            .and_then(|idx| idx.by_recv_owners.get(receiver_desc).cloned())
            .unwrap_or_default()
    }

    /// Rebuild the [`ExtCandidate`]s a facade/part `root` contributes for `name` — the lazy counterpart of
    /// the old eager index. Walks `root`'s super-class chain (each `ClassInfo` served from the L1/L2 cache),
    /// collecting matching statics; `public` mirrors the eager filter (a non-public root's public statics
    /// are the `@InlineOnly` splice-only candidates the inliner may select but resolution never emits).
    fn rebuild_ext_candidates(&self, root: &str, name: &str) -> Vec<ExtCandidate> {
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
                out.push(ExtCandidate {
                    owner: root.to_string(),
                    name: m.name.clone(),
                    descriptor: m.descriptor.clone(),
                    ret_desc,
                    signature: m.signature.clone(),
                    public: root_public && m.is_public(),
                });
            }
            cur = ci.super_class.clone();
        }
        out
    }

    /// A parsed `.kotlin_builtins` fragment by resource path (class internal name → supertypes+members),
    /// read once and cached. The single builtins entry point — both the collection hierarchy and a
    /// type's member API derive from it.
    fn builtins_file(
        &self,
        path: &str,
    ) -> std::rc::Rc<HashMap<String, super::metadata::BuiltinClass>> {
        if let Some(m) = self.builtins.borrow().get(path) {
            return m.clone();
        }
        let mut map = HashMap::new();
        for e in &self.entries {
            if let Entry::Jar(j) = e {
                if let Some(bytes) = self.jar_entry(j, path) {
                    map = super::metadata::parse_builtins(&bytes);
                    break;
                }
            }
        }
        let rc = std::rc::Rc::new(map);
        self.builtins
            .borrow_mut()
            .insert(path.to_string(), rc.clone());
        rc
    }

    /// The `.kotlin_builtins` fragment path for a package, mirroring kotlinc's
    /// `BuiltInSerializerProtocol.getBuiltInsFilePath`: `kotlin` → `kotlin/kotlin.kotlin_builtins`,
    /// `kotlin/collections` → `kotlin/collections/collections.kotlin_builtins`.
    fn builtins_path_for(internal: &str) -> String {
        let pkg = internal.rsplit_once('/').map_or("", |(p, _)| p);
        let last = pkg.rsplit_once('/').map_or(pkg, |(_, l)| l);
        format!("{pkg}/{last}.kotlin_builtins")
    }

    /// The parsed `collections.kotlin_builtins` fragment (the Kotlin collection hierarchy lives here).
    fn collection_builtins(&self) -> std::rc::Rc<HashMap<String, super::metadata::BuiltinClass>> {
        self.builtins_file("kotlin/collections/collections.kotlin_builtins")
    }

    /// Kotlin BUILTIN members (`String.length`, `List.get`, `Number.toInt`, …) as regular
    /// `LibraryMember` facts. The source name stays in `name`; JVM realization details stay in the JVM
    /// backend/provider and descriptor data.
    pub fn builtin_members(&self, internal: &str) -> Vec<crate::libraries::LibraryMember> {
        if let Some(members) = self.builtin_members.borrow_mut().get(internal) {
            cache_stat!(builtin_members, true);
            return members.as_ref().clone();
        }
        cache_stat!(builtin_members, false);
        let path = Self::builtins_path_for(internal);
        let f = self.builtins_file(&path);
        let members: Vec<_> = f
            .get(internal)
            .map(|class| {
                class.members.iter().map(|m| {
                    // A qualified Kotlin name (`kotlin/Int`, `kotlin/String`) → its JVM descriptor; a bare type
                    // parameter (`E`, `T` — no package) erases to `Object`.
                    let desc_of = |n: &str| -> String {
                        if n.contains('/') {
                            type_descriptor(kotlin_name_to_ty(n))
                        } else {
                            "Ljava/lang/Object;".to_string()
                        }
                    };
                    let pdesc: String = m.params.iter().map(|p| desc_of(p)).collect();
                    let descriptor = format!("({pdesc}){}", desc_of(&m.ret));
                    let ret = kotlin_name_to_ty(&m.ret);
                    let physical_ret = if m.ret.contains('/') {
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
                    let member_name = if m.is_property {
                        match m.name.as_str() {
                            // Kotlin/JVM mapped builtins whose property accessor is a plain Java method.
                            "length" | "size" | "values" => m.name.clone(),
                            "keys" => "keySet".to_string(),
                            "entries" => "entrySet".to_string(),
                            n if n.starts_with("is")
                                && n.as_bytes().get(2).is_some_and(|b| b.is_ascii_uppercase()) =>
                            {
                                n.to_string()
                            }
                            n => {
                                let mut c = n.chars();
                                format!(
                                    "get{}{}",
                                    c.next()
                                        .map(|f| f.to_uppercase().to_string())
                                        .unwrap_or_default(),
                                    c.as_str()
                                )
                            }
                        }
                    } else {
                        m.name.clone()
                    };
                    crate::libraries::LibraryMember {
                        name: member_name,
                        owner: Some(owner),
                        physical_name: None,
                        params: m.params.iter().map(|p| kotlin_name_to_ty(p)).collect(),
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
                    }
                })
            })
            .into_iter()
            .flatten()
            .collect();
        self.builtin_members
            .borrow_mut()
            .insert(internal.to_string(), std::rc::Rc::new(members.clone()));
        members
    }

    /// Whether the Kotlin builtin `internal` declares its function member `name`/`arity` with a NULLABLE
    /// return (`kotlin/collections/Map.get(K): V?`). A generic-return member is dropped from
    /// `builtin_members` (its return is a bare type parameter), and the member that actually resolves such
    /// a call is the erased classpath method (`java/util/Map.get` → `Object`) which carries no Kotlin
    /// nullability — so the builtin's `Type.nullable` flag is the only surviving record. `false` when no
    /// such member/builtin is recorded.
    pub fn builtin_member_ret_nullable(&self, internal: &str, name: &str, arity: usize) -> bool {
        let path = Self::builtins_path_for(internal);
        self.builtins_file(&path).get(internal).is_some_and(|c| {
            c.nullable_member_returns
                .iter()
                .any(|(n, a)| n == name && *a == arity)
        })
    }

    /// Direct supertypes declared in `.kotlin_builtins` for a Kotlin builtin class.
    pub fn builtin_supertypes(&self, internal: &str) -> Vec<String> {
        let path = Self::builtins_path_for(internal);
        self.builtins_file(&path)
            .get(internal)
            .map(|c| c.supertypes.clone())
            .unwrap_or_default()
    }

    /// The target internal name of the classpath `typealias` named `internal` (full name, e.g.
    /// `kotlin/collections/ArrayList` → `java/util/ArrayList`), or `None` if `internal` is not an alias.
    pub fn type_alias_target(&self, internal: &str) -> Option<String> {
        self.scan_types().type_aliases.get(internal).cloned()
    }

    /// Whether `internal` is a Kotlin BUILTIN declared in a `.kotlin_builtins` fragment (`kotlin/Number`,
    /// `kotlin/collections/List`, …), and if so whether it is an interface. `None` = not a builtin. Lets
    /// `resolve_type` report a builtin whose JVM class is absent (a no-JDK compile) from the builtins data,
    /// with the right class-vs-interface kind for member-invoke codegen.
    pub fn builtin_is_interface(&self, internal: &str) -> Option<bool> {
        let path = Self::builtins_path_for(internal);
        self.builtins_file(&path)
            .get(internal)
            .map(|c| c.is_interface)
    }

    /// Whether `internal` names a type in the Kotlin collection hierarchy (`collections.kotlin_builtins`)
    /// — i.e. one whose read-only/mutable identity is known here. A platform `java/util/List` or a user
    /// class is NOT (the front end never produces the former for a Kotlin collection; both keep their
    /// JVM-erased resolution).
    pub fn is_kotlin_collection(&self, internal: &str) -> bool {
        self.collection_builtins().contains_key(internal)
    }

    /// Whether `sub` is, or transitively is a subtype of, `sup` within the Kotlin collection hierarchy
    /// read from `collections.kotlin_builtins` (`MutableList <: MutableCollection`; `List` is NOT). The
    /// generic subtype query behind extension applicability — `MutableCollection.plusAssign` applies to a
    /// `MutableList` receiver but not a read-only `List`, exactly as kotlinc's overload resolution.
    pub fn kotlin_subtype(&self, sub: &str, sup: &str) -> bool {
        let f = self.collection_builtins();
        fn walk(
            map: &HashMap<String, super::metadata::BuiltinClass>,
            sub: &str,
            sup: &str,
        ) -> bool {
            sub == sup
                || map
                    .get(sub)
                    .is_some_and(|c| c.supertypes.iter().any(|s| walk(map, s, sup)))
        }
        walk(&f, sub, sup)
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
        let mut idx = TypeIndex::default();
        // Only Kotlin `*Kt` facades carry type aliases — scan jars/dirs for them. The JDK jimage has no
        // Kotlin metadata, so it is skipped entirely (it used to be walked only for the dead class-name map).
        for e in &self.entries {
            match e {
                Entry::Dir(d) => scan_types_dir(d, &mut idx),
                Entry::Jar(j) => scan_types_jar(j, &mut idx),
                Entry::Jimage(_) => {}
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
        let &(offset, size, compressed) = index.get(internal)?;
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
            None => (PathBuf::new(), std::sync::Arc::new(HashMap::new())),
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
        // The front end names built-in types in Kotlin terms (`kotlin/Any`); a classpath artifact is
        // a real JVM class, so map to the JVM name (`java/lang/Object`) before looking it up. The parsed
        // class is shared behind an `Arc`: L1↔L2 and every caller clone is a refcount bump, never a deep
        // copy of the (large) `ClassInfo`.
        let internal = super::jvm_class_map::to_jvm_internal(internal);
        // L1: per-thread, no lock.
        if let Some(hit) = self.local_cache.borrow_mut().get(internal) {
            cache_stat!(l1_class, true);
            return hit.clone();
        }
        cache_stat!(l1_class, false);
        // L2: process-global, shared across threads — a class parsed by ANY thread is reused here.
        if let Some(hit) = self.cache.read().unwrap().get(internal).cloned() {
            cache_stat!(l2_class, true);
            self.local_cache
                .borrow_mut()
                .insert(internal.to_string(), hit.clone());
            return hit;
        }
        cache_stat!(l2_class, false);
        let name = format!("{internal}.class");
        let mut found = None;
        for e in &self.entries {
            let bytes = match e {
                Entry::Dir(d) => std::fs::read(d.join(&name)).ok(),
                Entry::Jar(j) => self.jar_entry(j, &name),
                // The JDK jimage stores classes uncompressed — seek-read the class via a one-time
                // name→(offset,size) index so JDK type members (String, collections, …) resolve.
                Entry::Jimage(_) => self.jimage_bytes(internal),
            };
            if let Some(b) = bytes {
                if let Ok(ci) = parse_class(&b) {
                    found = Some(std::sync::Arc::new(ci));
                    break;
                }
            }
        }
        self.cache
            .write()
            .unwrap()
            .insert(internal.to_string(), found.clone());
        self.local_cache
            .borrow_mut()
            .insert(internal.to_string(), found.clone());
        found
    }

    /// The raw `.class` bytes for an internal name (Kotlin built-in names mapped to JVM first), or
    /// `None` if absent. Unlike `find`, this keeps the bytes (the inline expander needs the body).
    fn class_bytes(&self, internal: &str) -> Option<Vec<u8>> {
        let internal = super::jvm_class_map::to_jvm_internal(internal);
        let name = format!("{internal}.class");
        for e in &self.entries {
            let bytes = match e {
                Entry::Dir(d) => std::fs::read(d.join(&name)).ok(),
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
        let key = (
            internal.to_string(),
            name.to_string(),
            descriptor.to_string(),
        );
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
            let mut cur = self.find(internal).and_then(|ci| ci.super_class.clone());
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
                cur = self.find(&s).and_then(|ci| ci.super_class.clone());
            }
        }
        self.bodies.borrow_mut().insert(key, code.clone());
        code
    }

    /// Whether the selected JVM callable is `inline`, matching by `(jvm name, descriptor)` through the
    /// decoded Kotlin metadata. Use this once overload resolution has selected a concrete descriptor; it
    /// avoids a name-wide inline flag leaking from one overload to another.
    pub fn is_inline_callable(
        &self,
        internal: &str,
        name: &str,
        descriptor: &str,
        desc_params: &[Ty],
    ) -> bool {
        self.meta_functions(internal).iter().any(|f| {
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
                    .all(|(m, d)| ty_compat(&meta_param_ty(m.ty.as_deref()), d))
        })
    }

    /// Whether `internal.name(...)` is a Kotlin `suspend` function, per the class's `@Metadata`
    /// `IS_SUSPEND` flag (decoded once per class and cached). A call to it is a coroutine suspension
    /// point. Includes the multifile-facade part-class superclass walk.
    pub fn is_suspend_method(&self, internal: &str, name: &str) -> bool {
        if let Some(set) = self.suspend_names.borrow_mut().get(internal) {
            cache_stat!(suspend_names, true);
            return set.contains(name);
        }
        cache_stat!(suspend_names, false);
        let ci = self.find(internal);
        let mut names = ci
            .as_ref()
            .map(|c| super::metadata::suspend_method_names(c))
            .unwrap_or_default();
        let mut cur = ci.as_ref().and_then(|ci| ci.super_class.clone());
        while let Some(s) = cur {
            if s == "java/lang/Object" {
                break;
            }
            match self.find(&s) {
                Some(pci) => {
                    names.extend(super::metadata::suspend_method_names(&pci));
                    cur = pci.super_class.clone();
                }
                None => break,
            }
        }
        let set = std::rc::Rc::new(names);
        let hit = set.contains(name);
        self.suspend_names
            .borrow_mut()
            .insert(internal.to_string(), set);
        hit
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
        self.ext_by_name(method_name)
            .by_recv
            .get(receiver_desc)
            .cloned()
            .unwrap_or_default()
    }

    /// Every static method named `method_name` across the classpath (top-level functions and
    /// extensions), for resolving a receiver-less call. Includes non-public (`@InlineOnly`) candidates,
    /// each tagged via `ExtCandidate.public`; the caller filters — normal resolution is public-only.
    pub fn find_top_level(&self, method_name: &str) -> Vec<ExtCandidate> {
        self.ext_by_name(method_name).all.clone()
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
        let roots = self
            .ext
            .borrow()
            .as_ref()
            .and_then(|idx| idx.by_name.get(method_name).cloned())
            .unwrap_or_default();
        let mut grouped = ExtByName::default();
        for root in &roots {
            for cand in self.rebuild_ext_candidates(root, method_name) {
                if let Some(recv) = descriptor_parts(&cand.descriptor).and_then(|(fp, _)| fp) {
                    grouped.by_recv.entry(recv).or_default().push(cand.clone());
                }
                grouped.all.push(cand);
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
        // Built once per classpath process-wide (scanning every jar's statics is identical for a given
        // classpath) — shared across worker threads via the global cache, like the type/jimage indexes.
        let key: Vec<PathBuf> = self
            .entries
            .iter()
            .map(|e| e.path().to_path_buf())
            .collect();
        if let Some(idx) = global_ext_cache().lock().unwrap().get(&key) {
            *self.ext.borrow_mut() = Some(idx.clone());
            return;
        }
        // Serialize the (expensive, all-class-scanning) build so worker threads that start together on
        // the same classpath build the index ONCE, not N times: whoever wins the lock builds + caches;
        // the rest block, then hit the re-checked cache below. Without this the rayon threads all miss
        // the cache simultaneously at startup and each rescan the whole stdlib (~30% of compile time).
        static BUILD: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let _build = BUILD
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        if let Some(idx) = global_ext_cache().lock().unwrap().get(&key) {
            *self.ext.borrow_mut() = Some(idx.clone());
            return;
        }
        // Pass 1: collect a *lean* record per class — its `super_class` and its public static methods
        // (names+descriptors only, not the full `ClassInfo`). The stdlib's extension/top-level functions
        // live in package-private multifile *part* classes (`RangesKt___RangesKt`) that the public
        // facade (`RangesKt`) extends, so we need the parts even though they aren't public.
        let mut all: HashMap<String, ClassLite> = HashMap::new();
        for e in &self.entries {
            match e {
                Entry::Dir(d) => collect_dir(d, &mut all),
                Entry::Jar(j) => collect_jar(j, &mut all),
                // No Kotlin extensions live in the JDK.
                Entry::Jimage(_) => {}
            }
        }
        // Pass 2: index static methods reachable from each class. Public facade roots expose callable
        // statics (owner = facade, like kotlinc); non-public roots are kept too so private `@InlineOnly`
        // package-part functions can be selected as splice-only candidates. Those candidates are marked
        // non-public, so normal resolution never emits an illegal `invokestatic` to them.
        // Global `(name)` sets: declared as a genuine top-level function vs as an extension anywhere on the
        // classpath. A name that is ever an extension is NEVER excluded from the ext index; a name that is
        // ONLY ever top-level is excluded (its first parameter is a real value parameter, not a receiver).
        // Built across ALL classes so a multifile facade's bridge statics match the function metadata that
        // lives in its (separate) part classes.
        let global_toplevel: std::collections::HashSet<&str> = all
            .values()
            .flat_map(|c| c.toplevel_names.iter().map(|s| s.as_str()))
            .collect();
        let global_ext: std::collections::HashSet<&str> = all
            .values()
            .flat_map(|c| c.ext_names.iter().map(|s| s.as_str()))
            .collect();
        // Names that are top-level EVERYWHERE and never an extension — precomputed once (the same for
        // every occurrence, since the `@Metadata` classification is per name).
        let toplevel_only: std::collections::HashSet<String> = global_toplevel
            .iter()
            .filter(|n| !global_ext.contains(*n))
            .map(|s| s.to_string())
            .collect();
        let mut idx = ExtIndex {
            toplevel_only,
            ..ExtIndex::default()
        };
        // Record only WHERE each static lives (root class + receiver → owner) — the candidate records are
        // rebuilt on query from the root's `ClassInfo`, so the retained index is names + owners, not the
        // materialized statics.
        let push_dedup = |m: &mut HashMap<String, Vec<String>>, k: String, owner: &str| {
            let v = m.entry(k).or_default();
            if v.last().map(String::as_str) != Some(owner) && !v.iter().any(|o| o == owner) {
                v.push(owner.to_string());
            }
        };
        for (root, lite) in &all {
            let mut cur = Some(root.clone());
            let mut visited = std::collections::HashSet::new();
            while let Some(cn) = cur {
                if !visited.insert(cn.clone()) {
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
                    push_dedup(&mut idx.by_name, mname.clone(), root);
                    // Receiver keying: a genuine top-level (no first param, or a name that is top-level and
                    // never an extension) is NOT reachable via a receiver — skip `by_recv_owners` for it.
                    if let Some(first_param) = first_param {
                        if !idx.toplevel_only.contains(mname) {
                            push_dedup(&mut idx.by_recv_owners, first_param, root);
                        }
                    }
                }
                cur = c.super_class.clone();
            }
        }
        let idx = std::sync::Arc::new(idx);
        global_ext_cache().lock().unwrap().insert(key, idx.clone());
        *self.ext.borrow_mut() = Some(idx);
    }
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
    super_class: Option<String>,
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

fn collect_class_bytes(bytes: &[u8], all: &mut HashMap<String, ClassLite>) {
    let Ok(ci) = parse_class(bytes) else { return };
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
        ci.this_class.clone(),
        ClassLite {
            is_public: ci.is_public(),
            super_class: ci.super_class,
            statics,
            toplevel_names,
            ext_names,
        },
    );
}

fn collect_dir(dir: &Path, all: &mut HashMap<String, ClassLite>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_dir(&p, all);
        } else if p.extension().map_or(false, |x| x == "class") {
            if let Ok(b) = std::fs::read(&p) {
                collect_class_bytes(&b, all);
            }
        }
    }
}

fn collect_jar(jar: &Path, all: &mut HashMap<String, ClassLite>) {
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
            collect_class_bytes(&buf, all);
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
        idx.type_aliases.insert(alias, internal);
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

/// Build the jimage class index: internal name → [`JimageEntry`] (content offset + on-disk size +
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
    let mut idx = HashMap::new();
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
        idx.entry(internal)
            .or_insert((abs as u64, stored, comp != 0));
    }
    Some(idx)
}

#[cfg(test)]
mod fq_tests {
    use super::*;

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

    #[test]
    fn builtin_member_misses_are_cached() {
        let cp = Classpath::new(vec![PathBuf::from("/nonexistent/stdlib.jar")]);
        assert!(cp.builtin_members.borrow().is_empty());
        assert!(cp.builtin_members("kotlin/String").is_empty());
        assert!(cp.builtin_members.borrow().contains_key("kotlin/String"));
        assert!(cp.builtin_members("kotlin/String").is_empty());
    }
}
