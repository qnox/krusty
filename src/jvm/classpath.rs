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
use crate::jvm::names::{method_descriptor, parse_method_descriptor, type_descriptor};
use crate::libraries::{CallSig, GenericSig, ReturnInfo};
use crate::name_tree::{NameId, NameTree};
use crate::types::{type_name, type_name_from, Ty, TypeName, TypeNameList};

/// Resolve a JDK home to its `lib/modules` boot classpath.
///
/// An explicit home takes precedence over `JAVA_HOME`. Missing or invalid homes are a no-op so
/// callers can combine this with an explicit classpath without making environment setup mandatory.
pub fn platform_jdk_modules(jdk_home: Option<&Path>) -> Option<PathBuf> {
    let base = jdk_home
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("JAVA_HOME").map(PathBuf::from))?;
    let modules = base.join("lib").join("modules");
    modules.is_file().then_some(modules)
}

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
        "kotlin/UByte" => Ty::UByte,
        "kotlin/UShort" => Ty::UShort,
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
    } else if name.matches("kotlin/UByte") {
        Ty::UByte
    } else if name.matches("kotlin/UShort") {
        Ty::UShort
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

/// Interned ids for the names the hot `@Metadata` alignment paths test against, so per-parameter
/// checks are id compares instead of per-string name-tree walks.
struct MetaNameIds {
    prim: crate::name_tree::FxHashMap<crate::types::TypeName, Ty>,
    prim_array: crate::name_tree::FxHashMap<crate::types::TypeName, &'static str>,
    fn_arity: crate::name_tree::FxHashMap<crate::types::TypeName, usize>,
    array: crate::types::TypeName,
    any: crate::types::TypeName,
    object: crate::types::TypeName,
    string_kotlin: crate::types::TypeName,
    string_java: crate::types::TypeName,
    unit: crate::types::TypeName,
    nothing: crate::types::TypeName,
}

fn meta_ids() -> &'static MetaNameIds {
    static IDS: std::sync::OnceLock<MetaNameIds> = std::sync::OnceLock::new();
    IDS.get_or_init(|| {
        let tn = crate::types::type_name;
        let prim = [
            ("kotlin/Int", Ty::Int),
            ("kotlin/Char", Ty::Char),
            ("kotlin/Boolean", Ty::Boolean),
            ("kotlin/Long", Ty::Long),
            ("kotlin/Double", Ty::Double),
            ("kotlin/Float", Ty::Float),
            ("kotlin/Byte", Ty::Byte),
            ("kotlin/Short", Ty::Short),
            ("kotlin/UByte", Ty::UByte),
            ("kotlin/UShort", Ty::UShort),
            ("kotlin/UInt", Ty::UInt),
            ("kotlin/ULong", Ty::ULong),
        ]
        .into_iter()
        .map(|(name, ty)| (tn(name), ty))
        .collect();
        let prim_array = [
            ("kotlin/BooleanArray", "[Z"),
            ("kotlin/ByteArray", "[B"),
            ("kotlin/ShortArray", "[S"),
            ("kotlin/IntArray", "[I"),
            ("kotlin/LongArray", "[J"),
            ("kotlin/ULongArray", "[J"),
            ("kotlin/CharArray", "[C"),
            ("kotlin/FloatArray", "[F"),
            ("kotlin/DoubleArray", "[D"),
            ("kotlin/UIntArray", "[I"),
        ]
        .into_iter()
        .map(|(name, desc)| (tn(name), desc))
        .collect();
        let fn_arity = (0..=42)
            .map(|arity| (tn(&format!("kotlin/Function{arity}")), arity))
            .collect();
        MetaNameIds {
            prim,
            prim_array,
            fn_arity,
            array: tn("kotlin/Array"),
            any: tn("kotlin/Any"),
            object: tn("java/lang/Object"),
            string_kotlin: tn("kotlin/String"),
            string_java: tn("java/lang/String"),
            unit: tn("kotlin/Unit"),
            nothing: tn("kotlin/Nothing"),
        }
    })
}

fn meta_function_arity_name(name: TypeName) -> Option<usize> {
    if let Some(&arity) = meta_ids().fn_arity.get(&name) {
        return Some(arity);
    }
    name.unsigned_suffix_after_prefix("kotlin/Function")
}

fn primitive_array_descriptor_name(internal: TypeName) -> Option<&'static str> {
    meta_ids().prim_array.get(&internal).copied()
}

fn ty_erases_to_object(desc: Ty) -> bool {
    matches!(desc, Ty::Obj(n, _) if n == meta_ids().any || n == meta_ids().object)
}

/// The JVM representation recovered for a metadata-named value class. Keep unsigned normalization in
/// this single semantic adapter so top-level, member, exact, and compatible alignment cannot drift.
fn metadata_value_class_underlying(
    name: TypeName,
    nullable: bool,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<Ty> {
    if nullable {
        return None;
    }
    value_underlying(name).map(|underlying| {
        underlying
            .scalar_value_repr()
            .filter(|_| underlying.is_unsigned())
            .unwrap_or(underlying)
    })
}

/// The VALUE CLASS each descriptor parameter position really has, per `@Metadata`.
///
/// A `@JvmInline value class` erases to its underlying in the descriptor, so the parsed parameter is
/// `J`/`Ljava/lang/String;` while the source type is `Duration`/`Tag`. Overload selection compares the
/// ARGUMENT's Kotlin type against the parameter, so without this a call passing the value class matches
/// nothing. Only a position whose metadata names a value class AND whose descriptor carries exactly
/// that value class's underlying is reported — anything else stays `None` and the erased type stands.
///
/// A NULLABLE value class is boxed (the descriptor carries the class itself), so it needs no recovery;
/// `metadata_value_class_underlying` already returns `None` for it.
fn value_class_param_types(
    callable: &super::metadata::MetaFn,
    desc_params: &[Ty],
    extension: bool,
    kept: usize,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Vec<Option<Ty>> {
    let mut out = vec![None; desc_params.len()];
    // Descriptor layout: [extension receiver] [context params] [value params].
    let logical = kept.saturating_sub(usize::from(extension));
    let leading = logical.saturating_sub(callable.value_params.len());
    for (index, parameter) in callable.value_params.iter().enumerate() {
        let position = usize::from(extension) + leading + index;
        let (Some(name), Some(declared)) = (parameter.ty, desc_params.get(position)) else {
            continue;
        };
        let Some(underlying) =
            metadata_value_class_underlying(name, parameter.nullable(), value_underlying)
        else {
            continue;
        };
        if underlying.non_null() == declared.non_null() {
            // A value class krusty models as a SCALAR of its own is recovered as THAT scalar, never
            // as the boxed class. `UInt`/`ULong` ride in the JVM primitive slot of their carrier, so
            // a REFERENCE spelling here makes the lowerer box the argument (`kotlin/UInt.box-impl`)
            // into the erased descriptor slot (`I`/`J`) that takes it unboxed — a class file that
            // fails verification at load time. The scalar spelling is also the sharper one for the
            // overload selection this recovery exists for: it is exactly the type the checker gives
            // an unsigned argument. Every other value class (`Duration`, a user `Tag`) keeps the
            // class name: it is a reference on both sides, and the value-classes pass erases it at
            // the call.
            out[position] = Some(
                meta_ids()
                    .prim
                    .get(&name)
                    .copied()
                    .unwrap_or_else(|| Ty::obj_name(name)),
            );
        }
    }
    out
}

/// The VALUE CLASS a descriptor RETURN really has, per `@Metadata` — the return counterpart of
/// [`value_class_param_types`].
///
/// A value-class return erases exactly like a value-class parameter: the JVM method hands back the
/// UNDERLYING (`fun make(): K` → `make-<hash>()Ljava/lang/String;`) while `@Metadata` names `K`. The
/// call site needs BOTH halves. Knowing only the Kotlin return makes it treat the result as a BOXED
/// `K` and emit kotlinc's `checkcast K; K.unbox-impl()` over a `String` that is already the carrier —
/// a ClassCastException. Reporting the value class HERE is what marks the physical result as the
/// already-erased form, so the representation analysis leaves it alone.
///
/// Only a return whose metadata names a value class AND whose descriptor carries exactly that value
/// class's underlying is reported; anything else stays `None` and the erased type stands. A NULLABLE
/// value class is genuinely BOXED (the descriptor carries the class itself), and
/// `metadata_value_class_underlying` already returns `None` for it — so it keeps the boxed handling
/// it needs.
fn value_class_return_type(
    callable: &super::metadata::MetaFn,
    desc_ret: &Ty,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<Ty> {
    let name = callable.ret_class?;
    let underlying =
        metadata_value_class_underlying(name, callable.ret_nullable(), value_underlying)?;
    if underlying.non_null() != desc_ret.non_null() {
        return None;
    }
    // A value class krusty models as a SCALAR of its own is recovered as THAT scalar, never as the
    // boxed class — the same rule the parameter side applies, and for the same reason: `UInt`/`ULong`
    // ride in the JVM primitive slot of their carrier, so a REFERENCE spelling would make the lowerer
    // box a value the descriptor takes unboxed.
    Some(
        meta_ids()
            .prim
            .get(&name)
            .copied()
            .unwrap_or_else(|| Ty::obj_name(name)),
    )
}

/// Whether a `@Metadata` source value-parameter class name aligns with a JVM-descriptor parameter `Ty`.
/// This keeps the hot overload-alignment path in borrowed names: mapped builtins compare through
/// `to_jvm_internal`, arrays/functions use structural `Ty` facts, and no descriptor `String` is built just
/// to decide whether two class names denote the same erased JVM parameter.
fn meta_param_compat(
    name: Option<TypeName>,
    nullable: bool,
    desc: &Ty,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> bool {
    let Some(name) = name else {
        return desc.is_reference();
    };
    let ids = meta_ids();
    if let Some(arity) = meta_function_arity_name(name) {
        return matches!(desc, Ty::Fun(sig) if sig.params.len() == arity);
    }
    if name == ids.array || ids.prim_array.contains_key(&name) {
        return desc.is_array();
    }
    if let Some(prim) = ids.prim.get(&name) {
        if nullable {
            return desc.obj_internal().is_some_and(|actual| {
                crate::jvm::jvm_class_map::type_names_map_to_same_jvm_internal(actual, name)
            });
        }
        return match prim {
            // An unsigned parameter is metadata-compatible with its own name and with the signed
            // primitive it erases to (`UInt` <-> `Int`, `UByte` <-> `Byte`, …).
            u if u.is_unsigned() => *desc == *u || Some(*desc) == u.scalar_value_repr(),
            prim => desc == prim,
        };
    }
    // A value class erases to its UNDERLYING in the JVM descriptor (`kotlin/time/Duration` compiles to
    // `J`, `Tag(val v: String)` to `Ljava/lang/String;`), while `@Metadata` names the class itself —
    // admit the underlying, or every value-class-parametered function loses its metadata alignment
    // (parameter names, defaults, kept-param count). Decided BEFORE the by-descriptor arms below: a
    // REFERENCE underlying would otherwise be judged by the arm for its erasure (`Ty::String` asks only
    // whether the metadata name IS `String`) and rejected before ever reaching the value-class case.
    // The underlying normalizes like the mapped builtins above (`UInt` → `Int`).
    if let Some(erased) = metadata_value_class_underlying(name, nullable, value_underlying) {
        return erased.non_null() == desc.non_null()
            || (erased.is_reference() && desc.is_reference())
            || (ty_erases_to_object(*desc) && !desc.is_array());
    }
    if name == ids.unit {
        *desc == Ty::Unit
    } else if name == ids.nothing {
        *desc == Ty::Nothing
    } else if name == ids.any && desc.is_reference() {
        true
    } else if matches!(*desc, Ty::String) {
        name == ids.string_kotlin || name == ids.string_java
    } else if desc.obj_internal().is_some_and(|desc_internal| {
        crate::jvm::jvm_class_map::type_names_map_to_same_jvm_internal(desc_internal, name)
    }) {
        true
    } else {
        ty_erases_to_object(*desc) && !desc.is_array()
    }
}

fn meta_param_exact(
    name: Option<TypeName>,
    nullable: bool,
    desc: &Ty,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> bool {
    let Some(name) = name else {
        return ty_erases_to_object(*desc);
    };
    let ids = meta_ids();
    if let Some(arity) = meta_function_arity_name(name) {
        return matches!(desc, Ty::Fun(sig) if sig.params.len() == arity);
    }
    if name == ids.array {
        return matches!(desc, Ty::Obj(n, args)
            if *n == ids.array && args.first().copied().is_some_and(ty_erases_to_object));
    }
    if let Some(meta_desc) = primitive_array_descriptor_name(name) {
        return desc
            .obj_internal()
            .and_then(primitive_array_descriptor_name)
            == Some(meta_desc);
    }
    if let Some(prim) = ids.prim.get(&name) {
        if nullable {
            return desc.obj_internal().is_some_and(|actual| {
                crate::jvm::jvm_class_map::type_names_map_to_same_jvm_internal(actual, name)
            });
        }
        return match prim {
            // An unsigned parameter is metadata-compatible with its own name and with the signed
            // primitive it erases to (`UInt` <-> `Int`, `UByte` <-> `Byte`, …).
            u if u.is_unsigned() => *desc == *u || Some(*desc) == u.scalar_value_repr(),
            prim => desc == prim,
        };
    }
    // A value class erases to its underlying — `runTest(timeout: Duration)` aligns its metadata against
    // the erased `J` exactly only through it (unsigned underlyings normalize like the mapped builtins:
    // `UInt` → `Int`). Decided BEFORE the by-descriptor arms below, or a REFERENCE underlying is judged
    // by the arm for its erasure and rejected — see `meta_param_compat`.
    if let Some(erased) = metadata_value_class_underlying(name, nullable, value_underlying) {
        return erased.non_null() == desc.non_null();
    }
    if name == ids.unit {
        *desc == Ty::Unit
    } else if name == ids.nothing {
        *desc == Ty::Nothing
    } else if matches!(*desc, Ty::String) {
        name == ids.string_kotlin || name == ids.string_java
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct EntryKey {
    path: PathBuf,
    stamp: Option<EntryStamp>,
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
fn global_type_cache(
) -> &'static std::sync::Mutex<HashMap<Vec<EntryKey>, std::sync::Arc<TypeIndex>>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<Vec<EntryKey>, std::sync::Arc<TypeIndex>>>,
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
    resolved_types: CacheCounter,
    symbols_memo: CacheCounter,
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
            "class L1 {} · L2 {} | ext L1 {} · L2 {} | meta_fns {} | types {} | symbols {} | bodies {} | builtin {}",
            s.l1_class.line("hits"),
            s.l2_class.line("hits"),
            s.ext_l1.line("hits"),
            s.ext_l2.line("hits"),
            s.meta_fns.line("hits"),
            s.resolved_types.line("hits"),
            s.symbols_memo.line("hits"),
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
fn global_jimage_cache() -> &'static std::sync::Mutex<HashMap<EntryKey, std::sync::Arc<JimageIndex>>>
{
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<EntryKey, std::sync::Arc<JimageIndex>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn cached_jimage_index(path: &Path) -> Option<std::sync::Arc<JimageIndex>> {
    let key = EntryKey {
        path: path.to_path_buf(),
        stamp: entry_stamp(path),
    };
    let mut cache = global_jimage_cache().lock().unwrap();
    if let Some(index) = cache.get(&key) {
        return Some(index.clone());
    }
    let index = std::sync::Arc::new(build_jimage_index(path)?);
    cache.insert(key, index.clone());
    Some(index)
}

/// A process-global cache of a value derived from a SINGLE classpath entry (jar / dir / jimage), keyed
/// by that entry's path. A jar's classes, extension statics, and type aliases are identical wherever the
/// jar appears, so its contribution is built ONCE and shared by every classpath that includes it — a
/// classpath that only adds one library reuses every other entry's cached contribution and builds just
/// the new one. This is the composable layer UNDER the whole-classpath indexes: compose an index per cp
/// from these per-entry parts instead of rescanning every jar when the cp differs by a single entry.
struct EntryCache<T> {
    map: std::sync::Mutex<HashMap<EntryKey, std::sync::Arc<T>>>,
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
    fn get_or_build(&self, key: &EntryKey, build: impl FnOnce() -> T) -> std::sync::Arc<T> {
        self.get_or_build_if(key, build, |_| true)
    }

    fn get_or_build_if(
        &self,
        key: &EntryKey,
        build: impl FnOnce() -> T,
        cacheable: impl FnOnce(&T) -> bool,
    ) -> std::sync::Arc<T> {
        let mut map = self.map.lock().unwrap();
        if let Some(v) = map.get(key) {
            return v.clone();
        }
        let v = std::sync::Arc::new(build());
        if cacheable(&v) {
            map.insert(key.clone(), v.clone());
        }
        v
    }
}

fn push_id_dedup(m: &mut HashMap<String, Vec<NameId>>, key: &str, id: NameId) {
    let v = m.entry(key.to_string()).or_default();
    if v.last().copied() != Some(id) && !v.contains(&id) {
        v.push(id);
    }
}

/// Per-ENTRY extension-index contributions (one per jar/dir), unioned per queried name by
/// [`Classpath::ext_parts`]'s consumers. See [`EntryCache`].
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

type EntryPkgTypes = std::sync::Arc<TypeIndex>;
fn global_entry_pkg_types(
) -> &'static std::sync::Mutex<crate::lru::LruCache<(EntryKey, TypeName), EntryPkgTypes>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<crate::lru::LruCache<(EntryKey, TypeName), EntryPkgTypes>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        std::sync::Mutex::new(crate::lru::LruCache::new_fixed(GLOBAL_ALIAS_PACKAGE_CAP))
    })
}

/// The spec's `(jar, package) → PkgMembers`: a per-(jar, package) index of the package's static
/// callables, parsed once from that jar's `kotlin_module` facades and SHARED across every classpath that
/// includes the jar (keyed by jar path + package id), exactly like the other per-entry caches. Three
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
fn global_jar_pkg_members(
) -> &'static std::sync::Mutex<HashMap<(EntryKey, TypeName), JarPkgMembers>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<(EntryKey, TypeName), JarPkgMembers>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Process-global composed package table, keyed by the classpath entry set — like [`global_type_cache`],
/// the stdlib/JDK entries are identical across every compiled file, so the compose runs once per process.
fn global_pkg_tree_cache(
) -> &'static std::sync::Mutex<HashMap<Vec<EntryKey>, std::sync::Arc<PackageTree>>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<Vec<EntryKey>, std::sync::Arc<PackageTree>>>,
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
fn global_ext_candidates(key: &[EntryKey]) -> ExtCandCache {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<Vec<EntryKey>, ExtCandCache>>> =
        std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .entry(key.to_vec())
        .or_insert_with(|| std::sync::Arc::new(std::sync::RwLock::new(HashMap::new())))
        .clone()
}

/// Process-global cache of parsed `ClassInfo` (internal-name id → parsed class, `None` if absent from
/// THIS entry), keyed per classpath ENTRY (one jar/dir/jimage). The conformance harness compiles on
/// several rayon worker threads, EACH with its own `Classpath`, and some tests compose per-test
/// classpaths (a module output dir + the same stdlib jars); keying per entry — not per classpath set —
/// means the stdlib jars' parses are shared across ALL of those, so each class is parsed once per
/// process, period. `RwLock` because reads (cache hits) dominate; a parse on a miss takes the write
/// lock briefly.
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
fn global_entry_class_cache(key: &EntryKey) -> ClassCache {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<EntryKey, ClassCache>>> =
        std::sync::OnceLock::new();
    let m = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut g = m.lock().unwrap();
    g.entry(key.clone())
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

/// ONE classpath entry's contribution to the extension/top-level-function index, built once per
/// jar/dir (see [`EntryCache`]) and queried PER NAME by the ext lookups — there is no composed
/// whole-classpath index anymore. Composing one (`name → roots` over every stdlib name) was re-done
/// for every distinct classpath vector (each MODULE-test cp), and the queried working set is a tiny
/// fraction of it; the lookups now union the per-entry maps for just the queried name/receiver.
/// `by_recv_raw` stays UNFILTERED — the `toplevel_only` decision (`union(top) - union(ext)`) is
/// global across the whole cp, so it is applied per queried name at lookup time.
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

fn merge_alias_part(aliases: &mut TypeIndex, part: &TypeIndex) {
    for (&alias, &target) in &part.type_aliases {
        aliases.type_aliases.entry(alias).or_insert(target);
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
    /// The full source-declared return type selected from the SAME descriptor-aligned metadata
    /// callable as every other fact in this record. Unlike [`Self::ret`], this retains nested type
    /// arguments, so consumers do not repeat overload alignment merely to recover semantic
    /// classifiers erased by a JVM signature (`MutableList<MutableSet<T>>` → `List<Set<T>>`).
    pub declared_ret: Option<Ty>,
    /// Kotlin's source-level `operator` modifier. The JVM descriptor/name cannot encode it.
    pub is_operator: bool,
    /// The callable's declared contract, decoded from `@Metadata` (`None` when it has none).
    pub contract: Option<std::sync::Arc<crate::contracts::Contract>>,
    /// Leading context parameters (supplied implicitly by the caller, not positionally).
    pub context_count: usize,
    /// Per DESCRIPTOR parameter position, the VALUE CLASS `@Metadata` declares there when the JVM
    /// descriptor carries its erased underlying (`timeout: kotlin.time.Duration` ↔ `J`).
    ///
    /// The descriptor is the emit token and stays erased; resolution needs the Kotlin type, or a call
    /// passing a `Duration` is checked against `Long` and no overload is applicable. `None` at a
    /// position whose declared type is not a value class (the overwhelming majority).
    pub value_class_params: Vec<Option<Ty>>,
    /// The VALUE CLASS `@Metadata` declares as the RETURN when the JVM descriptor carries its erased
    /// underlying (`fun make(): K` ↔ `()Ljava/lang/String;`).
    ///
    /// The parameter facet above restores a Kotlin type resolution cannot otherwise see; this one
    /// additionally carries a CODEGEN fact — that the physical result is ALREADY the unboxed carrier.
    /// Without it a call site that knows the Kotlin return is `K` boxes as kotlinc does at a genuine
    /// box boundary and casts a `String` to `K`. `None` when the return is not a value class, or is a
    /// NULLABLE one (which really is boxed).
    pub value_class_ret: Option<Ty>,
}

impl MetadataCallFacts {
    fn fallback(call_sig: CallSig) -> Self {
        MetadataCallFacts {
            kept_params: None,
            call_sig,
            ret: ReturnInfo::default(),
            declared_ret: None,
            is_operator: false,
            contract: None,
            context_count: 0,
            value_class_params: Vec::new(),
            value_class_ret: None,
        }
    }
}

/// The per-function `@Metadata` lookups for one class, all derived from its single decoded function list
/// (facade parts merged). Computed once per class in [`Classpath::class_meta`].
struct ClassMeta {
    /// `(FxHash of jvm_name, flat index over the segment concatenation)`, sorted — the by-JVM-name
    /// lookup WITHOUT duplicating every function's name into a map key (one cloned `String` per
    /// function per cached class was ~46% of peak heap). Lookup hashes the queried name once, then
    /// binary-searches u64s ([`ClassMeta::fns_named`]) and VERIFIES each candidate by string
    /// equality (hash collisions stay correct). Same-name entries keep declaration order (sort is
    /// by `(hash, index)`).
    by_jvm_name: Vec<(u64, u32)>,
    /// The names under which a `suspend` function of this class can be looked up — BOTH its
    /// `@Metadata` source name and its (possibly mangled) JVM name, per [`suspend_lookup_names`].
    suspend_names: HashSet<String>,
    /// The facade's decoded [`MetaFn`] slices, SHARED by refcount: the class's own `Package`
    /// functions, or one segment per multifile PART. The parts' decodes are already retained on
    /// their cached `ClassInfo`s — segmenting instead of materializing a merged copy removes the
    /// duplicate `MetaFn`s (deep Strings included). Exposed via [`Classpath::meta_functions_name`]
    /// as an iterating handle, so lookups share THIS decode instead of re-merging `d1` themselves.
    fn_segments: Vec<std::sync::Arc<[super::metadata::MetaFn]>>,
    /// The facade's `@Metadata` property slices, segmented like `fn_segments`.
    prop_segments: Vec<std::sync::Arc<[super::metadata::MetaProp]>>,
}

impl ClassMeta {
    fn iter_fns(&self) -> impl Iterator<Item = &super::metadata::MetaFn> {
        self.fn_segments.iter().flat_map(|s| s.iter())
    }

    /// The function at FLAT index `i` over the segment concatenation.
    fn fn_at(&self, mut i: usize) -> &super::metadata::MetaFn {
        for s in &self.fn_segments {
            if i < s.len() {
                return &s[i];
            }
            i -= s.len();
        }
        unreachable!("ClassMeta::fn_at index out of range")
    }

    /// The flat indices whose `jvm_name` equals `name`: hash once, binary-search the u64 range,
    /// verify by string equality (collision-safe). Declaration order preserved.
    fn fns_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = u32> + 'a {
        let h = jvm_name_hash(name);
        let start = self.by_jvm_name.partition_point(|&(k, _)| k < h);
        self.by_jvm_name[start..]
            .iter()
            .take_while(move |&&(k, _)| k == h)
            .map(|&(_, i)| i)
            .filter(move |&i| self.fn_at(i as usize).jvm_name == name)
    }

    fn has_jvm_name(&self, name: &str) -> bool {
        self.fns_named(name).next().is_some()
    }
}

/// FxHash of a JVM method name, for [`ClassMeta::by_jvm_name`] — the compiler's trusted internal
/// data, so the dependency-lean hand-rolled hasher (see `name_tree`) is the right tool.
fn jvm_name_hash(name: &str) -> u64 {
    use std::hash::Hasher;
    let mut h = crate::name_tree::FxHasher::default();
    h.write(name.as_bytes());
    h.finish()
}

/// A refcounted handle to one class's decoded `@Metadata` functions (facade parts segmented) —
/// what [`Classpath::meta_functions_name`] returns so consumers iterate the shared decode.
pub struct MetaFns(std::rc::Rc<ClassMeta>);

impl MetaFns {
    pub fn iter(&self) -> impl Iterator<Item = &super::metadata::MetaFn> {
        self.0.iter_fns()
    }
}

/// The property analogue of [`MetaFns`].
pub struct MetaProps(std::rc::Rc<ClassMeta>);

impl MetaProps {
    pub fn iter(&self) -> impl Iterator<Item = &super::metadata::MetaProp> {
        self.0.prop_segments.iter().flat_map(|s| s.iter())
    }
}

#[derive(Default)]
struct BuiltinsFile {
    classes: HashMap<TypeName, BuiltinClass>,
}

struct BuiltinClass {
    supertypes: TypeNameList,
    /// The same supertypes carrying their type ARGUMENTS (`MutableList<E> : List<E>`) — the chain a
    /// receiver's type argument travels up when no JVM class generic signature is available.
    supertype_tys: Vec<Ty>,
    /// The class's formal type-parameter names, in declaration order (`Map` → `[K, V]`).
    formals: Vec<String>,
    members: Vec<BuiltinMember>,
    is_interface: bool,
    /// The builtin's own JVM class access flags, as an `InnerClasses` entry records them.
    access: u16,
    nullable_member_returns: Vec<(String, usize)>,
}

/// One decoded `.kotlin_builtins` member, in the ONE form that is not derivable: its declared
/// (unerased) signature — its own formals, parameter types and return, including type parameters
/// (`List<E>.get(Int): E`) and type arguments (`Set<Map.Entry<K, V>>`). A builtin member has no JVM
/// `Signature` string, so this is the only carrier that lets a type-parameter return bind against the
/// receiver's type arguments.
///
/// Deliberately NOT a [`crate::libraries::LibraryMember`]: the erased `params`/`ret`/`descriptor` a
/// `LibraryMember` carries are [`builtin_erased`] of this signature, and the rest of its fields need
/// the classpath (the `owner` mapping, the interface flag) which this decode does not have.
/// [`Classpath::builtin_members_name`] is where the two are joined, and it memoizes its result.
struct BuiltinMember {
    name: String,
    generic_sig: GenericSig,
    is_property: bool,
    ret_nullable: bool,
}

/// The JVM erasure of a decoded builtin type: a type parameter erases to `Any` (`Object`), a class to
/// itself with its type arguments dropped — exactly what a JVM descriptor records.
fn builtin_erased(ty: Ty) -> Ty {
    match ty {
        // JVM erasure follows the primary declared bound (`<T : CharSequence>` erases to
        // `CharSequence`), not unconditionally `Object`. Unbounded parameters already carry `Any?` as
        // their bound, so the same recursive rule covers both cases and stays aligned with
        // `names::type_descriptor` and bridge erasure.
        Ty::TyParam(_, bound) => builtin_erased(*bound),
        Ty::Nullable(inner) => builtin_erased(*inner),
        Ty::Obj(name, args) if !args.is_empty() => Ty::obj_name(name),
        other => other,
    }
}

/// The JVM descriptor a builtin member's declared signature erases to.
fn builtin_descriptor(sig: &GenericSig) -> String {
    let params: String = sig
        .params
        .iter()
        .map(|p| type_descriptor(builtin_erased(*p)))
        .collect();
    format!("({params}){}", type_descriptor(builtin_erased(sig.ret)))
}

/// A decoded `.kotlin_builtins` type as a [`Ty`]. `bounds` supplies each in-scope type parameter's
/// declared upper bound; an unlisted one is `Any?`, matching the `@Metadata` generic-signature decoder.
/// Nullability is applied in NESTED positions only — a top-level `T?` rides on the member's
/// `ret_nullable` flag, since the descriptor it pairs with erases it — again mirroring that decoder.
fn builtin_ty(t: &super::metadata::BuiltinTy, bounds: &HashMap<String, Ty>, nested: bool) -> Ty {
    use super::metadata::BuiltinTy;
    let ty = match t {
        BuiltinTy::Class { internal, args, .. } => {
            let name = type_name(internal);
            if args.is_empty() {
                kotlin_type_name_to_ty(name)
            } else {
                let args: Vec<Ty> = args.iter().map(|a| builtin_ty(a, bounds, true)).collect();
                Ty::obj_args_name(name, &args)
            }
        }
        BuiltinTy::Param { name, .. } => {
            let bound = bounds
                .get(name)
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            Ty::ty_param(name, bound)
        }
    };
    if nested && t.nullable() && matches!(ty, Ty::TyParam(..)) {
        Ty::nullable(ty)
    } else {
        ty
    }
}

/// The declared upper bound of each type parameter, keyed by name. Bounds are decoded with an EMPTY
/// bound map so a recursive bound (`E : Comparable<E>`) terminates.
fn builtin_bounds(
    params: &[super::metadata::BuiltinTypeParam],
    inherited: &HashMap<String, Ty>,
) -> HashMap<String, Ty> {
    let mut out = inherited.clone();
    for p in params {
        let bound = p
            .bounds
            .first()
            .map(|b| builtin_ty(b, &HashMap::new(), false))
            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
        out.insert(p.name.clone(), bound);
    }
    out
}

impl BuiltinsFile {
    fn from_classes(classes: HashMap<String, super::metadata::BuiltinClass>) -> Self {
        let mut file = BuiltinsFile::default();
        for (internal, class) in classes {
            let internal = type_name(&internal);
            let supertypes = class
                .supertypes
                .iter()
                .map(|name| type_name(name))
                .collect::<Vec<_>>()
                .into();
            let class_bounds = builtin_bounds(&class.type_params, &HashMap::new());
            let supertype_tys = class
                .supertype_tys
                .iter()
                .map(|t| builtin_ty(t, &class_bounds, false))
                .collect();
            let formals: Vec<String> = class.type_params.iter().map(|p| p.name.clone()).collect();
            let members = class
                .members
                .into_iter()
                .map(|m| {
                    let bounds = builtin_bounds(&m.formals, &class_bounds);
                    BuiltinMember {
                        name: m.name,
                        generic_sig: GenericSig {
                            formals: m.formals.iter().map(|p| p.name.clone()).collect(),
                            formal_bounds: m
                                .formals
                                .iter()
                                .map(|p| {
                                    p.bounds
                                        .iter()
                                        .map(|b| builtin_ty(b, &bounds, false))
                                        .collect()
                                })
                                .collect(),
                            receiver: None,
                            params: m
                                .params
                                .iter()
                                .map(|p| builtin_ty(p, &bounds, false))
                                .collect(),
                            ret: builtin_ty(&m.ret, &bounds, false),
                        },
                        is_property: m.is_property,
                        ret_nullable: m.ret_nullable,
                    }
                })
                .collect();
            file.classes.insert(
                internal,
                BuiltinClass {
                    supertypes,
                    supertype_tys,
                    formals,
                    members,
                    is_interface: class.is_interface,
                    access: class.access,
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
fn meta_callable_aligns(
    f: &super::metadata::MetaFn,
    desc_params: &[Ty],
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<(usize, usize)> {
    let off = f.is_extension() as usize;
    // Context parameters sit between the (extension) receiver and the value parameters in the
    // JVM descriptor; metadata keeps them out of `value_parameter` (field 13 instead), so the
    // aligned descriptor span is receiver + context + value params.
    let ctx = f.context_count;
    let end = off + ctx + f.value_params.len();
    if end > desc_params.len() {
        return None;
    }
    let receiver_ok = !f.is_extension()
        || match f.receiver_class {
            Some(rc) => meta_param_compat(Some(rc), false, &desc_params[0], value_underlying),
            None => desc_params[0].is_reference(),
        };
    if !receiver_ok
        || !f
            .value_params
            .iter()
            .zip(&desc_params[off + ctx..end])
            .all(|(m, d)| meta_param_compat(m.ty, m.nullable(), d, value_underlying))
    {
        return None;
    }
    let exact = f
        .value_params
        .iter()
        .zip(&desc_params[off + ctx..end])
        .filter(|(m, d)| meta_param_exact(m.ty, m.nullable(), d, value_underlying))
        .count();
    Some((end, exact))
}

/// The descriptor form of a metadata value parameter: a value class erases to its underlying
/// (`Duration` → `J`; unsigned normalizes like the mapped builtins, `UInt` → `I`) — except NULLABLE,
/// which boxes to the class itself. `actual` is the JVM descriptor segment; both forms are admitted.
/// [`type_descriptor`] is the single `Ty`-to-JVM boundary and already normalizes metadata's dotted
/// nested-class tail. Comparing its result directly is intentional: repeating that normalization in
/// this metadata-only caller previously let classpath matching carry a private descriptor policy that
/// bytecode emission did not share.
fn member_param_desc_matches(
    class: TypeName,
    nullable: bool,
    actual: &str,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> bool {
    let class_desc = type_descriptor(kotlin_type_name_to_ty(class));
    if class_desc == actual {
        return true;
    }
    let Some(erased) = metadata_value_class_underlying(class, nullable, value_underlying) else {
        return false;
    };
    type_descriptor(erased) == actual
}

fn metadata_member_descriptor(
    function: &super::metadata::MetaFn,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<String> {
    let signature = function.generic_sig.as_ref()?;
    let erased = |p: &Ty| {
        p.obj_internal()
            .and_then(|name| {
                metadata_value_class_underlying(name, p.is_nullable(), value_underlying)
            })
            .unwrap_or(*p)
    };
    let descriptor = if function.is_suspend() {
        let mut params: Vec<Ty> = signature.params.iter().map(erased).collect();
        params.push(Ty::obj("kotlin/coroutines/Continuation"));
        method_descriptor(&params, Ty::obj("kotlin/Any"))
    } else {
        let params: Vec<Ty> = signature.params.iter().map(erased).collect();
        method_descriptor(&params, signature.ret)
    };
    // `method_descriptor` delegates every component to the same normalized descriptor boundary used
    // by emission. Returning it unchanged prevents metadata members from maintaining a second,
    // provider-specific spelling repair.
    Some(descriptor)
}

fn metadata_member_shape_matches(
    function: &super::metadata::MetaFn,
    jvm_desc: &str,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> bool {
    let Some((params, ret)) = parse_method_descriptor(jvm_desc) else {
        return false;
    };
    let value_count = function.value_params.len();
    if params.len() != value_count + usize::from(function.is_suspend()) {
        return false;
    }
    if function
        .value_params
        .iter()
        .zip(&params)
        .any(|(parameter, actual)| match parameter.ty {
            Some(class) => {
                !member_param_desc_matches(class, parameter.nullable(), actual, value_underlying)
            }
            None => !actual.starts_with('L') && !actual.starts_with('['),
        })
    {
        return false;
    }
    if function.is_suspend() {
        return params.last() == Some(&"Lkotlin/coroutines/Continuation;")
            && ret == "Ljava/lang/Object;";
    }
    match function.ret_class {
        Some(class) => {
            member_param_desc_matches(class, function.ret_nullable(), ret, value_underlying)
        }
        None => ret.starts_with('L') || ret.starts_with('['),
    }
}

pub(super) fn aligned_member_metadata<'a>(
    functions: &'a [super::metadata::MetaFn],
    jvm_name: &str,
    jvm_desc: &str,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<&'a super::metadata::MetaFn> {
    let named = functions
        .iter()
        .filter(|function| function.jvm_name == jvm_name && !function.is_extension());
    let mut exact = named.clone().filter(|function| {
        function.jvm_desc == Some(jvm_desc)
            || (function.jvm_desc.is_none()
                && metadata_member_descriptor(function, value_underlying).as_deref()
                    == Some(jvm_desc))
    });
    if let Some(selected) = exact.next() {
        return exact.next().is_none().then_some(selected);
    }
    let mut compatible = named.filter(|function| {
        function.jvm_desc.is_none()
            && function.generic_sig.is_none()
            && metadata_member_shape_matches(function, jvm_desc, value_underlying)
    });
    let selected = compatible.next()?;
    compatible.next().is_none().then_some(selected)
}

/// Every name a `suspend` function answers to in [`ClassMeta::suspend_names`]: its `@Metadata` SOURCE
/// name and the JVM name it was compiled under. The two differ whenever the signature is mangled — a
/// value-class value parameter (`libU` → `libU-OzbTU-A`) or an explicit `@JvmName` — and a caller
/// keys by whichever name it holds. The top-level overload scan holds a BYTECODE method name, so a
/// source-only set left a mangled `suspend` function marked non-suspend: its descriptor kept the CPS
/// `Continuation`, nothing threaded it, and the emitted `invokestatic` was one argument short. The
/// sibling `jvm_name`-keyed lookups (inline-ness, the metadata call facts a contract rides on) match
/// the entry's JVM name already; indexing both names is what puts this one on the same footing.
///
/// Empty for a non-`suspend` function.
fn suspend_lookup_names(f: &super::metadata::MetaFn) -> impl Iterator<Item = String> + '_ {
    f.is_suspend()
        .then(|| {
            std::iter::once(f.kotlin_name.clone())
                .chain((f.jvm_name != f.kotlin_name).then(|| f.jvm_name.clone()))
        })
        .into_iter()
        .flatten()
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
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<(usize, usize)> {
    meta.fns_named(fn_name)
        .filter_map(|i| {
            let f = meta.fn_at(i as usize);
            let (end, exact) = meta_callable_aligns(f, desc_params, value_underlying)?;
            let ret_match = f.ret_class.is_some_and(|rc| {
                meta_param_compat(Some(rc), f.ret_nullable(), desc_ret, value_underlying)
            });
            Some((end, exact, ret_match, i))
        })
        .max_by_key(|(end, exact, ret_match, _)| (*end, *exact, *ret_match))
        .map(|(end, _, _, i)| (end, i as usize))
}

fn aligned_meta_callable<'a>(
    meta: &'a ClassMeta,
    fn_name: &str,
    desc_params: &[Ty],
    desc_ret: &Ty,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<(usize, &'a super::metadata::MetaFn)> {
    aligned_meta_index(meta, fn_name, desc_params, desc_ret, value_underlying)
        .map(|(end, i)| (end, meta.fn_at(i)))
}

pub(super) fn metadata_return_info(class: Option<TypeName>, nullable: bool) -> ReturnInfo {
    ReturnInfo::new(nullable, class.map(kotlin_type_name_to_ty))
}

/// Project the structured return from an already-selected metadata function. Keeping this beside the
/// lightweight [`ReturnInfo`] projection makes descriptor alignment the single overload decision:
/// callers may choose the cheap classifier/nullability view or the full generic structure without
/// searching the same metadata list again.
fn metadata_declared_return(function: &super::metadata::MetaFn) -> Option<Ty> {
    function
        .generic_sig
        .as_ref()
        .map(|signature| signature.ret)
        .or_else(|| function.ret_class.map(kotlin_type_name_to_ty))
}

/// Per-class `@Metadata` cache: class internal name → Kotlin function names that participate in
/// `@OverloadResolutionByLambdaReturnType` (`sumOf`, …). The resolver derives and verifies the concrete
/// JVM method (`sumOfInt`/`sumOfLong`/…) from the lambda return type, so the cache only needs membership.
type LambdaReturnOverloads = std::collections::HashSet<String>;
type MetaOverloadCache =
    RefCell<crate::lru::LruCache<TypeName, std::rc::Rc<LambdaReturnOverloads>>>;

const OPEN_ARCHIVE_CAP: usize = 16;
const ALIAS_PACKAGE_CAP: usize = 1024;
const GLOBAL_ALIAS_PACKAGE_CAP: usize = 8192;

pub struct Classpath {
    entries: Vec<Entry>,
    snapshot: Vec<Option<EntryStamp>>,
    cache_key: Vec<EntryKey>,
    // Two-level parsed-class cache: `local_cache` is a per-thread L1 (cheap `RefCell`, no lock —
    // serves the hot repeated lookups) holding the whole-classpath search result. Backing it,
    // `entry_caches` (parallel to `entries`) are process-global PER-ENTRY L2 caches (`RwLock`), so a
    // class is PARSED once per process — shared across worker threads AND across classpaths that
    // include the same jar. L1 miss → per-entry L2 walk in classpath order → parse.
    local_cache: RefCell<crate::lru::LruCache<TypeName, Option<std::sync::Arc<ClassInfo>>>>,
    entry_caches: Vec<ClassCache>,
    /// Open archives are hard-capped because each entry owns a file descriptor.
    archives: RefCell<crate::lru::LruCache<PathBuf, zip::ZipArchive<File>>>,
    /// Per-entry ext contributions (each cached process-globally by its path), fetched once per
    /// instance. The ext lookups union these per queried name — no composed whole-cp index.
    ext: RefCell<Option<std::rc::Rc<Vec<std::sync::Arc<EntryExt>>>>>,
    types: RefCell<Option<std::sync::Arc<TypeIndex>>>,
    /// Composed type aliases for recently queried packages.
    aliases: RefCell<crate::lru::LruCache<TypeName, std::sync::Arc<TypeIndex>>>,
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
    /// Memoized [`Self::facade_statics`] per facade root — `facade_method` walks the same facade's
    /// super chain for every extension emit-handle lookup, so the candidate vec is built once and
    /// shared behind `Rc`. Bounded by the queried facades (the working set).
    facade_statics_memo: RefCell<crate::lru::LruCache<TypeName, std::rc::Rc<Vec<ExtCandidate>>>>,
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
    /// Per-request classes that shadow filesystem entries.
    stub_overlay: RefCell<HashMap<TypeName, std::sync::Arc<ClassInfo>>>,
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
        let snapshot = entries
            .iter()
            .map(|entry| entry_stamp(entry.path()))
            .collect::<Vec<_>>();
        let cache_key = entries
            .iter()
            .zip(&snapshot)
            .map(|(entry, &stamp)| EntryKey {
                path: entry.path().to_path_buf(),
                stamp,
            })
            .collect::<Vec<_>>();
        // Per-cache LRU caps (entry counts). Sized ABOVE the conformance working set: entries are
        // Rc-shared records, so the practical bound is the queried vocabulary, and an undersized cap
        // thrashes (every eviction re-composes a type/namespace record or re-decodes metadata). The
        // caps still bound per-thread memory against a pathological vocabulary. Override all at once
        // with `KRUSTY_CACHE_CAP`.
        const CLASS_CAP: usize = 65536;
        const FN_CAP: usize = 65536;
        const META_CAP: usize = 65536;
        const BODY_CAP: usize = 2048;
        Classpath {
            entries,
            snapshot,
            cache_key: cache_key.clone(),
            local_cache: RefCell::new(crate::lru::LruCache::new(CLASS_CAP)),
            entry_caches: cache_key.iter().map(global_entry_class_cache).collect(),
            archives: RefCell::new(crate::lru::LruCache::new_fixed(OPEN_ARCHIVE_CAP)),
            ext: RefCell::new(None),
            types: RefCell::new(None),
            aliases: RefCell::new(crate::lru::LruCache::new_fixed(ALIAS_PACKAGE_CAP)),
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
            facade_statics_memo: RefCell::new(crate::lru::LruCache::new(META_CAP)),
            symbols_memo: RefCell::new(crate::lru::LruCache::new(FN_CAP)),
            id,
            stub_overlay: RefCell::new(HashMap::new()),
        }
    }

    /// Materialize indexes needed before source-specific name resolution.
    pub fn prepare_for_source_analysis(&self) {
        self.ensure_jimage_index();
        let _ = self.package_tree();
        let _ = self.ext_parts();
    }

    fn catalog_complete(&self) -> bool {
        self.package_tree().incomplete_entries.is_empty()
    }

    /// Whether all entries still match the snapshot captured at construction.
    pub fn snapshot_is_current(&self) -> bool {
        self.entries
            .iter()
            .zip(&self.snapshot)
            .all(|(entry, stamp)| entry_stamp(entry.path()) == *stamp)
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
        let aliases = self
            .aliases
            .borrow()
            .values()
            .map(|index| index.type_aliases.len())
            .sum::<usize>();
        let alias_packages = self.aliases.borrow().len();
        // RAW per-entry map sizes (unfiltered, undeduplicated) — the retained footprint, which is what
        // this memory diagnostic tracks; there is no composed per-cp index anymore.
        let ext = self.ext.borrow().as_ref().map_or(0, |parts| {
            parts
                .iter()
                .map(|p| p.by_name.len() + p.by_recv_raw.len())
                .sum()
        });
        let pkgtree = self
            .pkg_tree
            .borrow()
            .as_ref()
            .map_or(0, |t| t.package_count());
        format!(
            "classpath#{} L1_class={} L2_class={} meta_fns={} meta_ovl={} bodies={} builtin={} | \
             jimage={} type={} alias={} alias_pkg={} ext={} pkgtree={}",
            self.id,
            self.local_cache.borrow().len(),
            self.entry_caches.iter().map(|c| c.len()).sum::<usize>(),
            self.meta_fns.borrow().len(),
            self.meta_overloads.borrow().len(),
            self.bodies.borrow().len(),
            self.builtin_members.borrow().len(),
            jimage,
            types,
            aliases,
            alias_packages,
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
        let key = self.cache_key.clone();
        if let Some(t) = global_pkg_tree_cache().lock().unwrap().get(&key) {
            *self.pkg_tree.borrow_mut() = Some(t.clone());
            return t.clone();
        }
        let parts: Vec<std::sync::Arc<JarPackages>> = self
            .entries
            .iter()
            .enumerate()
            .map(|(entry_id, _)| self.entry_packages(entry_id))
            .collect();
        let tree = std::sync::Arc::new(compose_package_tree(&parts));
        if tree.incomplete_entries.is_empty() {
            global_pkg_tree_cache()
                .lock()
                .unwrap()
                .insert(key, tree.clone());
            *self.pkg_tree.borrow_mut() = Some(tree.clone());
        }
        tree
    }

    fn entry_packages(&self, entry_id: usize) -> std::sync::Arc<JarPackages> {
        global_jar_packages().get_or_build_if(
            &self.cache_key[entry_id],
            || build_jar_packages(&self.entries[entry_id]),
            |packages| packages.complete,
        )
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
        if !self.package_tree().incomplete_entries.is_empty() {
            cache_stat!(resolved_types, false);
            return None;
        }
        let hit = self.resolved_types.borrow_mut().get(&internal).cloned();
        cache_stat!(resolved_types, hit.is_some());
        hit
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
        if !self.package_tree().incomplete_entries.is_empty() {
            return;
        }
        self.resolved_types.borrow_mut().insert(internal, ty);
    }

    /// The decoded `@Metadata` function lookups for `internal` (facade parts merged), decoded once and
    /// cached. The single `d1` decode that `meta_functions`/`metadata_call_facts` all project over.
    fn class_meta(&self, internal: &str) -> std::rc::Rc<ClassMeta> {
        self.class_meta_name(type_name(internal))
    }

    fn class_meta_name(&self, internal_id: TypeName) -> std::rc::Rc<ClassMeta> {
        let catalog_complete = self.catalog_complete();
        if catalog_complete {
            if let Some(m) = self.meta_fns.borrow_mut().get(&internal_id) {
                cache_stat!(meta_fns, true);
                return m.clone();
            }
        }
        cache_stat!(meta_fns, false);
        let ci = self.find_name(internal_id);
        // SEGMENTS share every decoded slice by refcount — the class's own `Package` functions, or (for
        // a multifile FACADE, which has no function metadata of its own) each PART class's slice. The
        // parts' decodes are already retained on their cached `ClassInfo`s, so materializing a merged
        // copy here duplicated every part `MetaFn` (deep Strings included) — ~a third of peak heap.
        let mut fn_segments: Vec<std::sync::Arc<[super::metadata::MetaFn]>> = Vec::new();
        let mut prop_segments: Vec<std::sync::Arc<[super::metadata::MetaProp]>> = Vec::new();
        let mut suspend_names: HashSet<String> = HashSet::new();
        if let Some(ci) = &ci {
            if !ci.meta.package_functions.is_empty() {
                fn_segments.push(ci.meta.package_functions.clone());
            }
            if !ci.meta.package_properties.is_empty() {
                prop_segments.push(ci.meta.package_properties.clone());
            }
            suspend_names.extend(
                super::metadata::class_functions(ci)
                    .iter()
                    .flat_map(suspend_lookup_names),
            );
            // A multifile FACADE has no function/property metadata of its own — its `d1` lists the
            // PART class names, which hold them; each part slice becomes a shared segment (the same
            // fns-empty/props-empty gating the merged-copy version used).
            let merge_fns = ci.meta.package_functions.is_empty();
            let merge_props = ci.meta.package_properties.is_empty();
            if merge_fns || merge_props {
                for part in &ci.meta.multifile_parts {
                    let Some(pci) = self.find(part) else { continue };
                    if merge_fns && !pci.meta.package_functions.is_empty() {
                        fn_segments.push(pci.meta.package_functions.clone());
                    }
                    if merge_props && !pci.meta.package_properties.is_empty() {
                        prop_segments.push(pci.meta.package_properties.clone());
                    }
                    suspend_names.extend(
                        super::metadata::class_functions(&pci)
                            .iter()
                            .flat_map(suspend_lookup_names),
                    );
                }
            }
        }
        suspend_names.extend(
            fn_segments
                .iter()
                .flat_map(|s| s.iter())
                .flat_map(suspend_lookup_names),
        );
        // Hash-sorted by-JVM-name lookup over the flat segment concatenation: no name copies, and
        // same-name overloads stay in declaration order (sort by `(hash, index)`), matching the old
        // map's insertion-ordered index vecs.
        let mut by_jvm_name: Vec<(u64, u32)> = fn_segments
            .iter()
            .flat_map(|s| s.iter())
            .enumerate()
            .map(|(i, f)| (jvm_name_hash(&f.jvm_name), i as u32))
            .collect();
        by_jvm_name.sort_unstable();
        let meta = std::rc::Rc::new(ClassMeta {
            by_jvm_name,
            suspend_names,
            fn_segments,
            prop_segments,
        });
        if catalog_complete {
            self.meta_fns.borrow_mut().insert(internal_id, meta.clone());
        }
        meta
    }

    /// Every `@Metadata` function of `internal` (a facade's PART classes merged), decoded once and
    /// cached — the single source the metadata-primary `MetaFn` lookups share. Use this instead of
    /// re-calling `package_functions` + re-merging the facade parts at each call site.
    pub fn meta_functions(&self, internal: &str) -> MetaFns {
        MetaFns(self.class_meta(internal))
    }

    pub fn meta_functions_name(&self, internal: TypeName) -> MetaFns {
        MetaFns(self.class_meta_name(internal))
    }

    /// The facade-merged `@Metadata` properties of `internal`, from the same cached decode as
    /// [`Self::meta_functions_name`].
    pub fn meta_properties_name(&self, internal: TypeName) -> MetaProps {
        MetaProps(self.class_meta_name(internal))
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
        value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
    ) -> Option<Option<crate::libraries::GenericSig>> {
        let meta = self.class_meta_name(internal);
        meta.has_jvm_name(jvm_name).then(|| {
            aligned_meta_index(&meta, jvm_name, desc_params, desc_ret, value_underlying)
                .map(|(_, idx)| meta.fn_at(idx))
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
        value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
    ) -> MetadataCallFacts {
        self.metadata_call_facts_name(
            type_name(internal),
            fn_name,
            desc_params,
            desc_ret,
            extension,
            value_underlying,
        )
    }

    pub fn metadata_call_facts_name(
        &self,
        internal: TypeName,
        fn_name: &str,
        desc_params: &[Ty],
        desc_ret: &Ty,
        extension: bool,
        value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
    ) -> MetadataCallFacts {
        let meta = self.class_meta_name(internal);
        let Some((end, c)) =
            aligned_meta_callable(&meta, fn_name, desc_params, desc_ret, value_underlying)
        else {
            return MetadataCallFacts::fallback(if extension {
                CallSig::default()
            } else {
                CallSig::metadata_plain(desc_params.len())
            });
        };
        let names = c.value_params.iter().map(|p| p.name.clone()).collect();
        let defaults = c.value_params.iter().map(|p| p.has_default()).collect();
        let vararg = c.vararg_index();
        let mut call_sig = if extension {
            CallSig::metadata_extension(end, names, defaults, vararg)
        } else {
            let (lambda_receivers, lambda_receiver_params) = c.lambda_receiver_shape();
            CallSig::metadata_function(
                end,
                names,
                defaults,
                lambda_receivers,
                lambda_receiver_params,
                c.value_params.iter().map(|p| p.materialized()).collect(),
                vararg,
            )
        };
        let logical_param_count = end.saturating_sub(usize::from(extension));
        let leading_params = logical_param_count.saturating_sub(c.value_params.len());
        call_sig.platform_nullable_params = (0..leading_params)
            .map(|i| c.context_params_nullable.get(i).copied().unwrap_or(false))
            .chain(c.value_params.iter().map(|p| p.nullable()))
            .collect();
        MetadataCallFacts {
            kept_params: Some(end),
            call_sig,
            ret: metadata_return_info(c.ret_class, c.ret_nullable()),
            declared_ret: metadata_declared_return(c),
            is_operator: c.is_operator(),
            contract: c.contract.clone(),
            context_count: c.context_count,
            value_class_params: value_class_param_types(
                c,
                desc_params,
                extension,
                end,
                value_underlying,
            ),
            value_class_ret: value_class_return_type(c, desc_ret, value_underlying),
        }
    }

    pub fn metadata_member_call_facts_name(
        &self,
        internal: TypeName,
        jvm_name: &str,
        jvm_desc: &str,
        value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
    ) -> Option<MetadataCallFacts> {
        let ci = self.find_name(internal)?;
        let function = aligned_member_metadata(
            super::metadata::class_functions(&ci),
            jvm_name,
            jvm_desc,
            value_underlying,
        )?;
        Some(MetadataCallFacts {
            kept_params: None,
            call_sig: function.member_call_sig(),
            ret: metadata_return_info(function.ret_class, function.ret_nullable()),
            declared_ret: metadata_declared_return(function),
            is_operator: function.is_operator(),
            contract: function.contract.clone(),
            context_count: function.context_count,
            // Members recover their logical value-class parameters on their own path (the mangled-member
            // loop in `jvm_libraries`), so this facet stays empty here rather than duplicating it.
            value_class_params: Vec::new(),
            // The RETURN is NOT duplicated by that loop: it recovers the Kotlin return type but says
            // nothing about the physical result already being the erased carrier, which is the fact a
            // call site needs to skip the box. Derive it from the member's own descriptor.
            value_class_ret: parse_method_descriptor(jvm_desc).and_then(|(_, ret)| {
                value_class_return_type(
                    function,
                    &super::jvm_libraries::desc_to_ty(ret),
                    value_underlying,
                )
            }),
        })
    }

    /// The metadata-declared RETURN type of the PROPERTY getter realized by JVM method
    /// `jvm_name`/`jvm_desc`, with full structure when the metadata generic signature carries it and
    /// the bare classifier otherwise. Function returns travel in [`MetadataCallFacts::declared_ret`]
    /// from the already descriptor-aligned callable; a getter is not a metadata function, so this
    /// deliberately separate fallback matches its `JvmPropertySignature` without repeating function
    /// alignment. Together they carry collection identity that JVM signatures erase at every depth.
    pub fn metadata_property_ret_ty_name(
        &self,
        internal: TypeName,
        jvm_name: &str,
        jvm_desc: &str,
    ) -> Option<Ty> {
        let ci = self.find_name(internal)?;
        super::metadata::class_properties(&ci)
            .iter()
            .find(|property| {
                property
                    .getter
                    .as_ref()
                    .is_some_and(|getter| getter.name == jvm_name && getter.desc == jvm_desc)
            })
            .and_then(|property| {
                property
                    .generic_sig
                    .as_ref()
                    .map(|gsig| gsig.ret)
                    .or_else(|| property.ret_class.map(kotlin_type_name_to_ty))
            })
    }

    /// A facade class's lambda-return-overload Kotlin names, cached (part-merged for a multifile facade).
    pub fn lambda_return_overloads(&self, internal: &str) -> std::rc::Rc<LambdaReturnOverloads> {
        let internal_id = type_name(internal);
        let catalog_complete = self.catalog_complete();
        if catalog_complete {
            if let Some(m) = self.meta_overloads.borrow_mut().get(&internal_id) {
                return m.clone();
            }
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
        if catalog_complete {
            self.meta_overloads
                .borrow_mut()
                .insert(internal_id, rc.clone());
        }
        rc
    }

    /// Every distinct owner (facade) that declares a static method whose first parameter matches
    /// `receiver_desc` — the facades to consult for a Kotlin-name resolution (`sumOf`).
    pub fn find_extension_owners(&self, receiver_desc: &str) -> Vec<TypeName> {
        // Union the per-entry receiver records for THIS descriptor, dropping genuine top-level names
        // (never reachable via a receiver) — the same filter the composed index applied while merging.
        // Owners keep entry order and dedup across names/entries.
        let mut out: Vec<TypeName> = Vec::new();
        for p in self.ext_parts().iter() {
            let Some(statics) = p.by_recv_raw.get(receiver_desc) else {
                continue;
            };
            for (name, owner) in statics {
                if self.ext_toplevel_only(name) {
                    continue;
                }
                let owner = type_name_from(&p.owner_names, *owner);
                if !out.contains(&owner) {
                    out.push(owner);
                }
            }
        }
        out
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
        let tree = self.package_tree();
        let catalog_complete = tree.incomplete_entries.is_empty();
        if catalog_complete {
            if let Some(m) = self.builtins.borrow().get(&package) {
                return m.clone();
            }
        }
        let path = Self::builtins_path_for_package(package);
        let mut map = HashMap::new();
        let mut indices = tree
            .node_for(&package.render())
            .map_or_else(Vec::new, |node| node.builtins_jars.clone());
        indices.extend(tree.incomplete_entries.iter().copied());
        indices.sort_unstable();
        indices.dedup();
        for i in indices {
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            let bytes = match entry {
                Entry::Dir(dir) => std::fs::read(dir.join(&path)).ok(),
                Entry::Jar(jar) => self.jar_entry(jar, &path),
                Entry::Jimage(_) => None,
            };
            if let Some(bytes) = bytes {
                map = super::metadata::parse_builtins(&bytes);
                break;
            }
        }
        let rc = std::rc::Rc::new(BuiltinsFile::from_classes(map));
        if catalog_complete {
            self.builtins.borrow_mut().insert(package, rc.clone());
        }
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

    /// The nesting relation behind a mapped JVM name, when the enclosing JVM class has no class file to
    /// read its `InnerClasses` attribute off (no JDK on the classpath). Takes the nested JVM name
    /// (`java/util/Map$Entry`) and answers with the enclosing JVM name, the nested simple name, and the
    /// access flags an `InnerClasses` entry records — the same triple
    /// [`super::backend::classpath_inner_class_resolver`] otherwise reads off `java/util/Map.class`.
    ///
    /// The relation is not invented: a `$`-separated JVM name decomposes structurally, the enclosing
    /// half maps back to its Kotlin builtin, and the `.kotlin_builtins` fragment carries the nested
    /// declaration (`kotlin/collections/Map.Entry`) with its own flags. Requiring that declaration to
    /// exist is what keeps this from claiming a nesting relation for a `$` that is merely part of a
    /// mangled name.
    ///
    /// `None` for a non-nested name, a non-builtin enclosing type, or a nested name no builtin declares.
    pub fn builtin_nested_class(&self, jvm_internal: &str) -> Option<(String, String, u16)> {
        let (jvm_outer, simple) = jvm_internal.rsplit_once('$')?;
        let outer_id = type_name(jvm_outer);
        let kotlin_outer = super::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(outer_id)?;
        // A `.kotlin_builtins` fragment names a nested class with a DOTTED tail on the slashed package
        // (`kotlin/collections/Map.Entry`), so the package the fragment is looked up by stays the
        // enclosing class's package.
        let nested = type_name(&format!("{}.{simple}", kotlin_outer.render()));
        let file = self.builtins_file_for_package(Self::builtins_package_for(kotlin_outer));
        let class = file.get_name(nested)?;
        Some((jvm_outer.to_string(), simple.to_string(), class.access))
    }

    /// How reading a builtin PROPERTY is realized on its mapped JVM owner, when that owner has no class
    /// file to read the realization off (no JDK on the classpath). Takes the JVM owner
    /// (`java/util/List`) and the Kotlin property name (`size`, `keys`, `key`), and answers with the
    /// physical accessor the mapped `java.util`/`java.lang` type actually declares — the same
    /// name/descriptor/interface facts [`Self::builtin_members_name`] puts on the member, so the two
    /// cannot disagree. Walks the builtins supertype closure, because a property is often declared on a
    /// supertype (`List.size` on `Collection`).
    ///
    /// `None` for a non-builtin owner or a property no builtin declares — the caller then keeps its
    /// existing behaviour.
    fn builtin_property_read_access(
        &self,
        owner: &str,
        property: &str,
    ) -> Option<super::inline::PropertyAccess> {
        // Normalize to the JVM owner exactly as `inherited_property_access` does, so a read resolved
        // against the KOTLIN name (`kotlin/collections/List`) still dispatches on the mapped type and
        // never emits a reference to a class that does not exist at runtime.
        let jvm_owner_id = super::jvm_class_map::to_jvm_type_name(type_name(owner));
        let jvm_owner = jvm_owner_id.render();
        let kotlin = super::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(jvm_owner_id)
            .unwrap_or(jvm_owner_id);
        let mut queue = std::collections::VecDeque::from([kotlin]);
        let mut seen = std::collections::HashSet::new();
        while let Some(current) = queue.pop_front() {
            if !seen.insert(current) {
                continue;
            }
            let file = self.builtins_file_for_package(Self::builtins_package_for(current));
            let Some(class) = file.get_name(current) else {
                continue;
            };
            if let Some(member) = class
                .members
                .iter()
                .find(|m| m.is_property && m.name == property)
            {
                return Some(super::inline::PropertyAccess::Accessor {
                    // The dispatch owner stays the one the read was resolved against (mapped to its
                    // JVM name); only the accessor spelling and descriptor come from the declaring
                    // builtin, exactly as an inherited class-file accessor keeps the receiver's owner.
                    owner: jvm_owner.clone(),
                    name: builtin_property_jvm_name(&member.name),
                    // The member's OWN descriptor, which is already erased (`Map.Entry.key: K` is
                    // `()Ljava/lang/Object;`). Rebuilding it from the use-site logical type would emit
                    // `getKey:()Ljava/lang/String;`, a method no class declares.
                    descriptor: builtin_descriptor(&member.generic_sig),
                    is_static: false,
                    is_interface: class.is_interface,
                });
            }
            queue.extend(class.supertypes.iter_ids());
        }
        None
    }

    /// Kotlin BUILTIN members (`String.length`, `List.get`, `Number.toInt`, …) as regular
    /// `LibraryMember` facts. The source name stays in `name`; JVM realization details stay in the JVM
    /// backend/provider and descriptor data.
    pub fn builtin_members(&self, internal: &str) -> Vec<crate::libraries::LibraryMember> {
        self.builtin_members_name(type_name(internal))
    }

    pub fn builtin_members_name(
        &self,
        internal_id: TypeName,
    ) -> Vec<crate::libraries::LibraryMember> {
        let catalog_complete = self.catalog_complete();
        if catalog_complete {
            if let Some(members) = self.builtin_members.borrow_mut().get(&internal_id) {
                cache_stat!(builtin_members, true);
                return members.as_ref().clone();
            }
        }
        cache_stat!(builtin_members, false);
        let f = self.builtins_file_for_package(Self::builtins_package_for(internal_id));
        let members: Vec<_> = f
            .get_name(internal_id)
            .map(|class| {
                class.members.iter().map(|m| {
                    // A `LibraryMember` states the member in its ERASED, JVM-descriptor shape (the form
                    // a classpath member arrives in, and the form overload alignment compares against);
                    // the declared shape rides along in `generic_sig`. Both are the one decoded builtin
                    // signature, erased here.
                    let descriptor = builtin_descriptor(&m.generic_sig);
                    let params: Vec<Ty> = m
                        .generic_sig
                        .params
                        .iter()
                        .map(|p| builtin_erased(*p))
                        .collect();
                    let ret = builtin_erased(m.generic_sig.ret);
                    let physical_ret = ret;
                    // The owner's JVM class: the kotlin↔JVM map (`kotlin/String` → `java/lang/String`), and for the
                    // non-collection mapped builtins (`kotlin/CharSequence` → `java/lang/CharSequence`, …) the
                    // emit-only simple-name mapping — the member virtual-dispatches on that JVM type.
                    let owner = crate::jvm::jvm_class_map::to_jvm_type_name(internal_id);
                    // Interface dispatch: prefer the real class flag, else the builtin's OWN
                    // `.kotlin_builtins` `CLASS_KIND` — a Kotlin builtin and the JVM class it maps to
                    // always agree on interface-ness (`List`/`java.util.List`, `Number`/`java.lang
                    // .Number`), and every member here comes from a builtins entry that carries the flag
                    // — so no curated per-name table is needed (the old fallback covered a handful of
                    // names and answered `false` for every `java/util/*`, emitting `invokevirtual` on an
                    // interface).
                    let is_iface = self
                        .find_name(owner)
                        .map(|ci| ci.is_interface())
                        .unwrap_or(class.is_interface);
                    let member_name = if m.is_property {
                        builtin_property_jvm_name(&m.name)
                    } else {
                        m.name.clone()
                    };
                    crate::libraries::LibraryMember {
                        name: member_name,
                        owner: Some(owner),
                        physical_name: None,
                        params,
                        ret,
                        physical_ret,
                        descriptor,
                        signature: None,
                        // A builtin member carries no JVM `Signature` string, so its DECODED signature
                        // is the only record of a type-parameter return/parameter — without it a
                        // generic member would resolve with an `Any`-erased return.
                        generic_sig: Some(m.generic_sig.clone()),
                        // `ret_nullable` — the declared return nullability from the `.kotlin_builtins`
                        // `Type.nullable` flag (`Map.get(K): V?`); the JVM descriptor erases it.
                        flags: crate::libraries::LmFlags::default()
                            .with_ret_nullable(m.ret_nullable)
                            .with_is_interface(is_iface),
                        inline: crate::libraries::InlineKind::None,
                        // Builtin (`.kotlin_builtins`) members are all public API.
                        visibility: crate::libraries::Visibility::Public,
                        call_sig: crate::libraries::CallSig::metadata_plain(
                            m.generic_sig.params.len(),
                        ),
                        // No `.kotlin_builtins` member declares a value-class return: the builtins are
                        // the mapped platform types, whose members predate value classes entirely.
                        declared_ret: None,
                    }
                })
            })
            .into_iter()
            .flatten()
            .collect();
        if catalog_complete {
            self.builtin_members
                .borrow_mut()
                .insert(internal_id, std::rc::Rc::new(members.clone()));
        }
        members
    }

    /// Whether the Kotlin builtin `internal` declares its function member `name`/`arity` with a NULLABLE
    /// return (`kotlin/collections/Map.get(K): V?`). When the mapped JVM class IS on the classpath the
    /// member that resolves such a call is the erased classpath method (`java/util/Map.get` → `Object`),
    /// which carries no Kotlin nullability — so the builtin's `Type.nullable` flag is the only surviving
    /// record. `false` when no such member/builtin is recorded.
    pub fn builtin_member_ret_nullable(&self, internal: &str, name: &str, arity: usize) -> bool {
        self.builtin_member_ret_nullable_name(type_name(internal), name, arity)
    }

    pub fn builtin_member_ret_nullable_name(
        &self,
        internal_id: TypeName,
        name: &str,
        arity: usize,
    ) -> bool {
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

    /// The `.kotlin_builtins` analogue of a class generic signature: the builtin's formal
    /// type-parameter names and its supertypes WITH type arguments (`MutableList<E> : List<E>`). This
    /// is what lets a receiver's type argument bind (and travel up the hierarchy) when the mapped JVM
    /// class — whose `Signature` normally carries these facts — is absent from the classpath.
    /// `internal` may be the Kotlin name or its mapped JVM form (`java/util/List`).
    pub fn builtin_class_gsig_name(&self, internal: TypeName) -> Option<(Vec<String>, Vec<Ty>)> {
        let kotlin =
            super::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(internal).unwrap_or(internal);
        self.builtins_file_for_package(Self::builtins_package_for(kotlin))
            .get_name(kotlin)
            .map(|c| (c.formals.clone(), c.supertype_tys.clone()))
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
        let tree = self.package_tree();
        if !tree.incomplete_entries.is_empty() {
            return self.scan_types().type_aliases.get(&internal).copied();
        }
        let package = internal.parent().unwrap_or_else(|| type_name(""));
        if let Some(index) = self.aliases.borrow_mut().get(&package) {
            return index.type_aliases.get(&internal).copied();
        }

        let mut index = TypeIndex::default();
        let rendered = package.render();
        if let Some(node) = tree.node_for(&rendered) {
            for &entry_id in &node.jars {
                merge_alias_part(&mut index, &self.entry_package_types(entry_id, package));
            }
        }
        let index = std::sync::Arc::new(index);
        let target = index.type_aliases.get(&internal).copied();
        self.aliases.borrow_mut().insert(package, index);
        target
    }

    fn entry_package_types(&self, entry_id: usize, package: TypeName) -> EntryPkgTypes {
        let key = (self.cache_key[entry_id].clone(), package);
        let packages = self.entry_packages(entry_id);
        if packages.complete {
            if let Some(index) = global_entry_pkg_types().lock().unwrap().get(&key) {
                return index.clone();
            }
        }
        let index = std::sync::Arc::new(build_entry_package_types(
            &self.entries[entry_id],
            &packages,
            package,
        ));
        if packages.complete {
            global_entry_pkg_types()
                .lock()
                .unwrap()
                .insert(key, index.clone());
        }
        index
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
        Classpath::new(Vec::new())
    }

    /// Scan all classpath entries and return the full type index (class names + type aliases).
    /// Cached per-instance after the first call, and **process-globally** keyed by the entry paths —
    /// so scanning the JDK jimage (the whole `java.base`) happens once per process, not once per
    /// compiled file (which dominated box-suite wall time).
    /// The classpath's type index, shared via `Arc` so per-file callers pay a pointer bump, not a
    /// deep clone of the (large) class-name/alias maps. Cached per-instance and process-globally.
    pub fn scan_types(&self) -> std::sync::Arc<TypeIndex> {
        let tree = self.package_tree();
        let catalog_complete = tree.incomplete_entries.is_empty();
        if catalog_complete {
            if let Some(idx) = self.types.borrow().as_ref() {
                return idx.clone();
            }
        }
        let key = self.cache_key.clone();
        if catalog_complete {
            if let Some(idx) = global_type_cache().lock().unwrap().get(&key) {
                *self.types.borrow_mut() = Some(idx.clone());
                return idx.clone();
            }
        }
        // Compose from per-ENTRY alias tables. Each entry's scan (parse every `*Kt` facade for type
        // aliases) is built ONCE via `EntryCache` — which holds its map lock across the build — and shared
        // by every classpath that includes the jar. So the expensive scan no longer races across all
        // worker threads on cold start (it dominated `resolve_type` in the flamegraph via
        // `type_alias_target`); only the cheap map merge runs per classpath. This mirrors the ext index's
        // per-entry composition (d8bbc91).
        let mut idx = TypeIndex::default();
        for (entry_id, e) in self.entries.iter().enumerate() {
            let packages = self.entry_packages(entry_id);
            let part = if tree.incomplete_entries.contains(&entry_id) {
                std::sync::Arc::new(build_entry_types(e, &packages))
            } else {
                global_entry_types().get_or_build(&self.cache_key[entry_id], || {
                    build_entry_types(e, &packages)
                })
            };
            for (&alias, &target) in &part.type_aliases {
                // First entry on the classpath wins — kotlinc/java class-resolution order (and this doc's
                // "first hit" invariant). The old inline scan `insert`ed in entry order, so a LATER jar
                // overwrote an earlier one (last-wins); that was a latent divergence, masked only because
                // no two corpus jars declare the same alias (box conformance stays FAIL:0 either way).
                idx.type_aliases.entry(alias).or_insert(target);
            }
        }
        let idx = std::sync::Arc::new(idx);
        if catalog_complete {
            global_type_cache().lock().unwrap().insert(key, idx.clone());
            *self.types.borrow_mut() = Some(idx.clone());
        }
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
            Some(p) => cached_jimage_index(&p).map(|index| (p, index)),
            None => Some((PathBuf::new(), std::sync::Arc::new(JimageIndex::default()))),
        };
        if let Some(entry) = entry {
            *self.jimage.borrow_mut() = Some(entry);
        }
    }

    fn jar_entry(&self, jar: &Path, name: &str) -> Option<Vec<u8>> {
        let mut archives = self.archives.borrow_mut();
        if !archives.contains_key(jar) {
            let file = File::open(jar).ok()?;
            let archive = zip::ZipArchive::new(file).ok()?;
            archives.insert(jar.to_path_buf(), archive);
        }
        let archive = archives.get_mut(jar)?;
        let mut entry = archive.by_name(name).ok()?;
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut buf).ok()?;
        Some(buf)
    }

    pub fn find(&self, internal: &str) -> Option<std::sync::Arc<ClassInfo>> {
        self.find_name(type_name(internal))
    }

    fn class_entry_indices(&self, tree: &PackageTree, internal: &str) -> Vec<usize> {
        let mut indices = tree.jars_for_class(internal);
        indices.extend(tree.incomplete_entries.iter().copied());
        indices.sort_unstable();
        indices.dedup();
        indices
    }

    pub fn find_name(&self, internal: TypeName) -> Option<std::sync::Arc<ClassInfo>> {
        // The front end names built-in types in Kotlin terms (`kotlin/Any`); a classpath artifact is
        // a real JVM class, so map to the JVM name (`java/lang/Object`) before looking it up. The parsed
        // class is shared behind an `Arc`: L1↔L2 and every caller clone is a refcount bump, never a deep
        // copy of the (large) `ClassInfo`.
        let internal_id = super::jvm_class_map::to_jvm_type_name(internal);
        if let Some(hit) = self.stub_overlay.borrow().get(&internal_id) {
            return Some(hit.clone());
        }
        let tree = self.package_tree();
        let catalog_complete = tree.incomplete_entries.is_empty();
        // L1: per-thread, no lock.
        let l1_hit = self.local_cache.borrow_mut().get(&internal_id).cloned();
        if let Some(hit) = l1_hit {
            // A recovered earlier entry may shadow a cached hit from a later entry.
            if catalog_complete {
                cache_stat!(l1_class, true);
                return hit;
            }
        }
        cache_stat!(l1_class, false);
        let internal = internal_id.render();
        let name = format!("{internal}.class");
        let mut found = None;
        let mut all_cached = true;
        for i in self.class_entry_indices(&tree, &internal) {
            let (Some(e), Some(l2)) = (self.entries.get(i), self.entry_caches.get(i)) else {
                continue;
            };
            let incomplete = tree.incomplete_entries.contains(&i);
            // L2: process-global per-entry cache — a class parsed from this jar/dir by ANY thread
            // (under ANY classpath that includes it) is reused; `None` records "absent from this
            // entry", so the classpath-order walk still stops at the first entry that owns the class.
            match l2.classes.read().unwrap().get(&internal_id).cloned() {
                Some(Some(hit)) if !incomplete => {
                    found = Some(hit);
                    break;
                }
                Some(None) if !incomplete => continue,
                None | Some(_) => {}
            }
            all_cached = false;
            let bytes = match e {
                Entry::Dir(d) => std::fs::read(d.join(&name)).ok(),
                Entry::Jar(j) => self.jar_entry(j, &name),
                // The JDK jimage stores classes uncompressed — seek-read the class via a one-time
                // name→(offset,size) index so JDK type members (String, collections, …) resolve.
                Entry::Jimage(_) => self.jimage_bytes(&internal),
            };
            // A DIRECTORY entry on a case-INSENSITIVE filesystem (macOS APFS) happily serves
            // `java/lang/error.class` for `Error.class` — verify the parsed class IS the
            // requested one (JVM names are case-sensitive; `error` must not resolve to `Error`).
            let parsed = bytes
                .and_then(|b| parse_class(&b).ok())
                .filter(|ci| ci.this_class_matches(&internal))
                .map(std::sync::Arc::new);
            if parsed.is_some() || !incomplete {
                l2.classes
                    .write()
                    .unwrap()
                    .insert(internal_id, parsed.clone());
            }
            if let Some(ci) = parsed {
                found = Some(ci);
                break;
            }
        }
        cache_stat!(l2_class, all_cached);
        if catalog_complete {
            self.local_cache
                .borrow_mut()
                .insert(internal_id, found.clone());
        }
        found
    }

    /// Replace the in-memory class overlay and invalidate dependent lookups.
    pub fn set_stub_overlay(&self, classes: Vec<(String, Vec<u8>)>) {
        let mut map = HashMap::new();
        for (_, bytes) in classes {
            if let Ok(ci) = parse_class(&bytes) {
                map.entry(ci.this_class)
                    .or_insert_with(|| std::sync::Arc::new(ci));
            }
        }
        *self.stub_overlay.borrow_mut() = map;
        self.clear_overlay_memos();
    }

    fn clear_overlay_memos(&self) {
        self.local_cache.borrow_mut().clear();
        self.resolved_types.borrow_mut().clear();
        self.symbols_memo.borrow_mut().clear();
    }

    /// Remove all in-memory classes.
    pub fn clear_stub_overlay(&self) {
        if !self.stub_overlay.borrow().is_empty() {
            self.stub_overlay.borrow_mut().clear();
            self.clear_overlay_memos();
        }
    }

    /// Return the jar containing `internal`, if its first classpath definition is in a jar.
    pub fn owning_jar(&self, internal: &str) -> Option<PathBuf> {
        let internal_id = super::jvm_class_map::to_jvm_type_name(type_name(internal));
        if self.stub_overlay.borrow().contains_key(&internal_id) {
            return None;
        }
        let (index, _) = self.physical_class_entry(&internal_id.render())?;
        match self.entries.get(index)? {
            Entry::Jar(path) => Some(path.clone()),
            Entry::Dir(_) | Entry::Jimage(_) => None,
        }
    }

    /// The class or builtins jar whose attached sources declare `internal`.
    pub fn declaring_jar(&self, internal: &str) -> Option<PathBuf> {
        let name = type_name(internal);
        if self.builtin_is_interface_name(name).is_none() {
            return self.owning_jar(internal);
        }
        let package = Self::builtins_package_for(name);
        let path = Self::builtins_path_for_package(package);
        let tree = self.package_tree();
        let mut indices = tree
            .node_for(&package.render())
            .map_or_else(Vec::new, |node| node.builtins_jars.clone());
        indices.extend(tree.incomplete_entries.iter().copied());
        indices.sort_unstable();
        indices.dedup();
        indices
            .into_iter()
            .find_map(|index| match self.entries.get(index) {
                Some(Entry::Jar(jar)) if self.jar_entry(jar, &path).is_some() => Some(jar.clone()),
                _ => None,
            })
    }

    /// The raw `.class` bytes for an internal name (Kotlin built-in names mapped to JVM first), or
    /// `None` if absent. Unlike `find`, this keeps the bytes (the inline expander needs the body).
    fn class_bytes(&self, internal: &str) -> Option<Vec<u8>> {
        let internal = super::jvm_class_map::to_jvm_internal(internal);
        self.physical_class_entry(internal).map(|(_, bytes)| bytes)
    }

    fn physical_class_entry(&self, internal: &str) -> Option<(usize, Vec<u8>)> {
        let name = format!("{internal}.class");
        let tree = self.package_tree();
        for index in self.class_entry_indices(&tree, internal) {
            let e = self.entries.get(index)?;
            let bytes = match e {
                Entry::Dir(d) => std::fs::read(d.join(&name)).ok().filter(|b| {
                    // Case-insensitive-filesystem guard (see `find`): the served file must BE the
                    // requested class, not a case-collided sibling.
                    parse_class(b).is_ok_and(|ci| ci.this_class_matches(internal))
                }),
                Entry::Jar(j) => self.jar_entry(j, &name),
                Entry::Jimage(_) => self.jimage_bytes(internal),
            };
            if let Some(bytes) = bytes {
                return Some((index, bytes));
            }
        }
        None
    }

    /// Lazily read (and cache) one method's bytecode body — the inline expander's entry point. Each
    /// `(class, method, descriptor)` body is read and parsed at most once, even across many call sites.
    pub fn method_code(&self, internal: &str, name: &str, descriptor: &str) -> Option<MethodCode> {
        let internal_id = type_name(internal);
        let key = (internal_id, name.to_string(), descriptor.to_string());
        let catalog_complete = self.catalog_complete();
        if catalog_complete {
            if let Some(hit) = self.bodies.borrow_mut().get(&key) {
                cache_stat!(bodies, true);
                return hit.clone();
            }
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
        if catalog_complete {
            self.bodies.borrow_mut().insert(key, code.clone());
        }
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
        value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
    ) -> bool {
        self.meta_functions_name(internal).iter().any(|f| {
            if !f.is_inline() || f.jvm_name != name {
                return false;
            }
            if f.jvm_desc == Some(descriptor) {
                return true;
            }
            if f.jvm_desc.is_some() {
                return false;
            }
            let off = f.is_extension() as usize;
            let end = off + f.value_params.len();
            end == desc_params.len()
                && f.value_params
                    .iter()
                    .zip(&desc_params[off..end])
                    .all(|(m, d)| meta_param_compat(m.ty, m.nullable(), d, value_underlying))
        })
    }

    /// Whether `internal.name(...)` is a Kotlin `suspend` function, per the class's `@Metadata`
    /// `IS_SUSPEND` flag. A call to it is a coroutine suspension point. Includes the superclass walk
    /// needed for facade part classes. `name` may be either the source name or the compiled JVM name
    /// (see [`suspend_lookup_names`]); a caller holding a bytecode method name must still strip a
    /// `$default` suffix, which is a synthetic with no `@Metadata` entry of its own.
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
        // A genuine top-level name is never reachable via a receiver.
        if self.ext_toplevel_only(method_name) {
            return Vec::new();
        }
        // O(1): the rebuilt candidates are pre-grouped by receiver (first-parameter) descriptor.
        self.ext_by_name(method_name).render_by_recv(receiver_desc)
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
            .iter()
            .filter(|c| c.name == jvm_name)
            .cloned()
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
    /// [`Self::pkg_members`] in one pass. Each `ClassInfo` is served from the L1/L2 cache. Memoized per
    /// root behind `Rc`: `facade_method` re-walks the same facade for every extension emit-handle lookup,
    /// and rebuilding the candidate vec re-interned every owner name and re-split every descriptor.
    fn facade_statics(&self, root: &str) -> std::rc::Rc<Vec<ExtCandidate>> {
        let root_id = type_name(root);
        let catalog_complete = self.package_tree().incomplete_entries.is_empty();
        if catalog_complete {
            if let Some(hit) = self.facade_statics_memo.borrow_mut().get(&root_id) {
                return hit.clone();
            }
        }
        let rc = std::rc::Rc::new(self.build_facade_statics(root));
        if catalog_complete {
            self.facade_statics_memo
                .borrow_mut()
                .insert(root_id, rc.clone());
        }
        rc
    }

    fn build_facade_statics(&self, root: &str) -> Vec<ExtCandidate> {
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
    /// classpaths (keyed by jar path + package id in [`global_jar_pkg_members`]). Roots at the PUBLIC facade
    /// (`CollectionsKt`), not the package-private multifile PART (`CollectionsKt__…`) `kotlin_module`
    /// lists — the callable public statics live on the facade, and `facade_statics` drops a non-public
    /// root's public statics (the `@InlineOnly` rule).
    fn jar_pkg_members_name(&self, entry_id: usize, pkg: TypeName) -> JarPkgMembers {
        let entry_key = &self.cache_key[entry_id];
        let key = (entry_key.clone(), pkg);
        let jp = self.entry_packages(entry_id);
        if jp.complete {
            let g = global_jar_pkg_members().lock().unwrap();
            if let Some(m) = g.get(&key) {
                return m.clone();
            }
        }
        let pkg_rendered = pkg.render();
        let mut m = PkgMembers::default();
        if let Some(pe) = jp.entry(&pkg_rendered) {
            let mut seen_facade = std::collections::HashSet::new();
            let mut roots: Vec<String> = Vec::new();
            for &part_id in &pe.facades {
                let part = jp.names.render(part_id);
                // The public multifile facade is the `__`-prefix of a part (`…/CollectionsKt__X` →
                // `…/CollectionsKt`); a single-file facade has no `__` and roots at itself. Root at
                // BOTH: the facade carries the callable public statics, but an `@InlineOnly` inline
                // function's body is a PRIVATE static on the package-private PART only (never
                // forwarded), and the bytecode inliner must still find it — `facade_statics` keeps a
                // non-public root's non-public statics for exactly that, with `public: false` so
                // resolution admits them only as splice-only candidates.
                if let Some((facade, _)) = part.split_once("__") {
                    if seen_facade.insert(facade.to_string()) {
                        roots.push(facade.to_string());
                    }
                }
                if seen_facade.insert(part.clone()) {
                    roots.push(part);
                }
            }
            for facade in &roots {
                let facade = facade.as_str();
                let facade_id = m.owner_names.insert(facade);
                let metas = self.meta_functions(facade);
                for cand in self.facade_statics(facade).iter() {
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
                        .push(ExtCandidateRecord::from_candidate(facade_id, cand));
                    m.by_jvm.entry(jvm_name).or_default().push(idx);
                    m.by_source.entry(source).or_default().push(idx);
                }
            }
        }
        let rc = std::sync::Arc::new(m);
        if jp.complete {
            global_jar_pkg_members()
                .lock()
                .unwrap()
                .insert(key, rc.clone());
        }
        rc
    }

    /// The PUBLIC multifile facades a package declares, from the `kotlin_module` catalog (the parts
    /// `…Kt__X` collapsed to their public facade `…Kt`), across every jar that declares the package. The
    /// `@Metadata`-driven extension/top-level discovery reads each facade's merged metadata — the source
    /// of truth — instead of scanning JVM statics. Declaration order, deduped.
    pub fn package_facades(&self, pkg: &str) -> Vec<TypeName> {
        self.package_facades_name(type_name(pkg))
    }

    pub fn package_facades_name(&self, pkg: TypeName) -> Vec<TypeName> {
        let tree = self.package_tree();
        let mut out = Vec::new();
        let pkg_rendered = pkg.render();
        let Some(node) = tree.node_for(&pkg_rendered) else {
            return out;
        };
        for &jar_id in &node.jars {
            if self.entries.get(jar_id).is_none() {
                continue;
            }
            let jp = self.entry_packages(jar_id);
            if let Some(pe) = jp.entry(&pkg_rendered) {
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
        packages: &[TypeName],
    ) -> Vec<ExtCandidate> {
        let tree = self.package_tree();
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for &pkg in packages {
            if !seen.insert(pkg) {
                continue;
            }
            let pkg_rendered = pkg.render();
            let Some(node) = tree.node_for(&pkg_rendered) else {
                continue;
            };
            for &jar_id in &node.jars {
                if self.entries.get(jar_id).is_none() {
                    continue;
                }
                let members = self.jar_pkg_members_name(jar_id, pkg);
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
    pub fn extension_owners_in_scope(
        &self,
        recv_desc: &str,
        packages: &[TypeName],
    ) -> Vec<TypeName> {
        let tree = self.package_tree();
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut seen_owner = std::collections::HashSet::new();
        for &pkg in packages {
            if !seen.insert(pkg) {
                continue;
            }
            let pkg_rendered = pkg.render();
            let Some(node) = tree.node_for(&pkg_rendered) else {
                continue;
            };
            for &jar_id in &node.jars {
                if self.entries.get(jar_id).is_none() {
                    continue;
                }
                let members = self.jar_pkg_members_name(jar_id, pkg);
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
        scope: Option<&[TypeName]>,
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
        scope: Option<&[TypeName]>,
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
    pub fn functions_in_scope(&self, name: &str, packages: &[TypeName]) -> Vec<ExtCandidate> {
        let tree = self.package_tree();
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for &pkg in packages {
            if !seen.insert(pkg) {
                continue;
            }
            let pkg_rendered = pkg.render();
            let Some(node) = tree.node_for(&pkg_rendered) else {
                continue;
            };
            for &jar_id in &node.jars {
                if self.entries.get(jar_id).is_none() {
                    continue;
                }
                let members = self.jar_pkg_members_name(jar_id, pkg);
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
        if !self.package_tree().incomplete_entries.is_empty() {
            cache_stat!(symbols_memo, false);
            return None;
        }
        let hit = self.symbols_memo.borrow_mut().get(&fqn).cloned();
        cache_stat!(symbols_memo, hit.is_some());
        hit
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
        if self.package_tree().incomplete_entries.is_empty() {
            self.symbols_memo.borrow_mut().insert(fqn, rc.clone());
        }
        rc
    }

    /// The memoized rebuild for `method_name`, shared across threads and grouped by receiver — a hot name
    /// (`map`, `let`) is walked once for the whole process, and both `find_top_level` and `find_extensions`
    /// are then O(1) reads. This is what makes the lazy index perform: the WHERE map is tiny + retained;
    /// candidate records are rebuilt on first use and kept only for queried names.
    fn ext_by_name(&self, method_name: &str) -> std::sync::Arc<ExtByName> {
        let catalog_complete = self.package_tree().incomplete_entries.is_empty();
        // L1: per-thread, no lock — the hot resolver path.
        if catalog_complete {
            if let Some(hit) = self.ext_l1.borrow_mut().get(method_name).cloned() {
                cache_stat!(ext_l1, true);
                return hit;
            }
        }
        cache_stat!(ext_l1, false);
        // L2: process-global, shared across threads (the rebuild happens once here).
        if catalog_complete {
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
        }
        cache_stat!(ext_l2, false);
        // Union the per-entry root lists for THIS name (entry order, dedup by rendered root — the same
        // order the composed index's per-part merge produced), then rebuild candidates from each root's
        // cached `ClassInfo`, grouped by receiver.
        let mut grouped = ExtByName::default();
        let mut seen_roots: Vec<String> = Vec::new();
        for p in self.ext_parts().iter() {
            let Some(owners) = p.by_name.get(method_name) else {
                continue;
            };
            for &owner_id in owners {
                let root = p.owner_names.render(owner_id);
                if seen_roots.contains(&root) {
                    continue;
                }
                let owner = grouped.owner_names.insert_from(&p.owner_names, owner_id);
                for cand in self.rebuild_ext_candidate_records(owner, &root, method_name) {
                    let cand_idx = grouped.all.len();
                    if let Some(recv) = descriptor_parts(&cand.descriptor).and_then(|(fp, _)| fp) {
                        grouped.by_recv.entry(recv).or_default().push(cand_idx);
                    }
                    grouped.all.push(cand);
                }
                seen_roots.push(root);
            }
        }
        let rc = std::sync::Arc::new(grouped);
        if catalog_complete {
            self.ext_candidates
                .write()
                .unwrap()
                .insert(method_name.to_string(), rc.clone());
            self.ext_l1
                .borrow_mut()
                .insert(method_name.to_string(), rc.clone());
        }
        rc
    }

    /// The per-entry ext contributions, each scanned once per process (cached by entry path via
    /// `global_entry_ext`) and fetched once per instance.
    fn ext_parts(&self) -> std::rc::Rc<Vec<std::sync::Arc<EntryExt>>> {
        let tree = self.package_tree();
        let catalog_complete = tree.incomplete_entries.is_empty();
        if catalog_complete {
            if let Some(parts) = self.ext.borrow().as_ref() {
                return parts.clone();
            }
        }
        let parts: std::rc::Rc<Vec<std::sync::Arc<EntryExt>>> = std::rc::Rc::new(
            self.entries
                .iter()
                .enumerate()
                .map(|(entry_id, e)| {
                    let packages = self.entry_packages(entry_id);
                    if tree.incomplete_entries.contains(&entry_id) {
                        std::sync::Arc::new(build_entry_ext(e, &packages))
                    } else {
                        global_entry_ext().get_or_build(&self.cache_key[entry_id], || {
                            build_entry_ext(e, &packages)
                        })
                    }
                })
                .collect(),
        );
        if catalog_complete {
            *self.ext.borrow_mut() = Some(parts.clone());
        }
        parts
    }

    /// Whether `@Metadata` marks `name` as a GENUINE top-level function — top-level in some entry and
    /// an extension in none — so `find_extensions` must not return it for any receiver. The cross-entry
    /// union is evaluated per queried name instead of materializing a whole-cp set.
    fn ext_toplevel_only(&self, name: &str) -> bool {
        let parts = self.ext_parts();
        parts.iter().any(|p| p.toplevel_names.contains(name))
            && !parts.iter().any(|p| p.ext_names.contains(name))
    }
}

impl Default for Classpath {
    fn default() -> Self {
        Self::empty()
    }
}

/// Scan ONE classpath entry into its [`EntryExt`] contribution: collect each class's lean record, then
/// index the statics reachable via each class's super-walk WITHIN this entry (a Kotlin multifile facade
/// and its `*___*Kt` part classes are compiled into the same jar, so the chain never crosses entries).
/// The `toplevel_only` filter is a whole-classpath decision, so it is deferred to the per-name lookup
/// ([`Classpath::ext_toplevel_only`]) — here every receiver-taking static is recorded raw in `by_recv_raw`.
fn build_entry_ext(entry: &Entry, packages: &JarPackages) -> EntryExt {
    let mut names = NameTree::default();
    let mut all: HashMap<NameId, ClassLite> = HashMap::new();
    match entry {
        Entry::Dir(d) => collect_dir(d, &mut names, &mut all),
        Entry::Jar(j) => collect_jar(j, packages, &mut names, &mut all),
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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct EntryStamp {
    len: u64,
    modified_ns: Option<u128>,
    path_identity: u64,
}

fn entry_stamp(path: &Path) -> Option<EntryStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.is_dir() {
        return Some(directory_stamp(path));
    }
    Some(EntryStamp {
        len: metadata.len(),
        modified_ns: modified_ns(&metadata),
        path_identity: path_identity(path),
    })
}

fn modified_ns(metadata: &std::fs::Metadata) -> Option<u128> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
}

fn hash_stamp(hash: &mut u64, bytes: &[u8]) {
    const FNV_PRIME: u64 = 0x100000001b3;
    for &byte in bytes {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn enter_directory(
    path: &Path,
    ancestors: &mut HashSet<PathBuf>,
) -> std::io::Result<Option<PathBuf>> {
    let canonical = std::fs::canonicalize(path)?;
    Ok(ancestors.insert(canonical.clone()).then_some(canonical))
}

fn path_identity(path: &Path) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const SYMLINK: &[u8] = b"\0symlink";
    const SYMLINK_METADATA_FAILED: &[u8] = b"\0symlink-metadata-failed";
    const READ_LINK_FAILED: &[u8] = b"\0read-link-failed";
    let mut hash = FNV_OFFSET;
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        hash_stamp(&mut hash, SYMLINK_METADATA_FAILED);
        return hash;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hash_stamp(&mut hash, &metadata.dev().to_le_bytes());
        hash_stamp(&mut hash, &metadata.ino().to_le_bytes());
        hash_stamp(&mut hash, &metadata.ctime().to_le_bytes());
        hash_stamp(&mut hash, &metadata.ctime_nsec().to_le_bytes());
    }
    #[cfg(not(unix))]
    {
        let created_ns = metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        hash_stamp(&mut hash, &created_ns.to_le_bytes());
    }
    if !metadata.file_type().is_symlink() {
        return hash;
    }
    hash_stamp(&mut hash, SYMLINK);
    hash_stamp(&mut hash, &metadata.len().to_le_bytes());
    hash_stamp(
        &mut hash,
        &modified_ns(&metadata).unwrap_or_default().to_le_bytes(),
    );
    match std::fs::read_link(path) {
        Ok(target) => hash_stamp(&mut hash, target.as_os_str().as_encoded_bytes()),
        Err(_) => hash_stamp(&mut hash, READ_LINK_FAILED),
    }
    hash
}

fn directory_stamp(root: &Path) -> EntryStamp {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const CANONICALIZE_FAILED: &[u8] = b"\0canonicalize-failed";
    const READ_DIR_FAILED: &[u8] = b"\0read-dir-failed";
    const READ_ENTRY_FAILED: &[u8] = b"\0read-entry-failed";
    const METADATA_FAILED: &[u8] = b"\0metadata-failed";
    let mut hash = FNV_OFFSET;
    let mut total_len = 0u64;
    let mut stack = vec![(root.to_path_buf(), HashSet::new())];
    while let Some((directory, mut ancestors)) = stack.pop() {
        match enter_directory(&directory, &mut ancestors) {
            Ok(Some(_)) => {}
            Ok(None) => continue,
            Err(_) => {
                hash_stamp(&mut hash, CANONICALIZE_FAILED);
                hash_stamp(&mut hash, directory.as_os_str().as_encoded_bytes());
                continue;
            }
        }
        let Ok(read_dir) = std::fs::read_dir(&directory) else {
            hash_stamp(&mut hash, READ_DIR_FAILED);
            hash_stamp(&mut hash, directory.as_os_str().as_encoded_bytes());
            continue;
        };
        let mut paths = Vec::new();
        let mut failed_entries = 0u64;
        for entry in read_dir {
            match entry {
                Ok(entry) => paths.push(entry.path()),
                Err(_) => failed_entries += 1,
            }
        }
        if failed_entries != 0 {
            hash_stamp(&mut hash, READ_ENTRY_FAILED);
            hash_stamp(&mut hash, &failed_entries.to_le_bytes());
        }
        paths.sort_unstable();
        for path in paths {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            hash_stamp(&mut hash, relative.as_os_str().as_encoded_bytes());
            hash_stamp(&mut hash, &path_identity(&path).to_le_bytes());
            let Ok(metadata) = std::fs::metadata(&path) else {
                hash_stamp(&mut hash, METADATA_FAILED);
                continue;
            };
            hash_stamp(&mut hash, &metadata.len().to_le_bytes());
            hash_stamp(
                &mut hash,
                &modified_ns(&metadata).unwrap_or_default().to_le_bytes(),
            );
            if metadata.file_type().is_dir() {
                stack.push((path, ancestors.clone()));
            } else {
                total_len = total_len.wrapping_add(metadata.len());
            }
        }
    }
    EntryStamp {
        len: total_len,
        modified_ns: Some(u128::from(hash)),
        path_identity: path_identity(root),
    }
}

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
    /// Exact internal class names declared by this entry.
    classes: Vec<NameId>,
    /// Whether the entire entry was catalogued successfully.
    complete: bool,
}

impl JarPackages {
    fn entry(&self, pkg: &str) -> Option<&PkgEntry> {
        self.names.get(pkg).and_then(|id| self.packages.get(&id))
    }

    fn entry_mut(&mut self, pkg: &str) -> &mut PkgEntry {
        let id = self.names.insert(pkg);
        self.packages.entry(id).or_default()
    }

    fn contains_facade(&self, internal: &str) -> bool {
        let package = internal.rsplit_once('/').map_or("", |(package, _)| package);
        let Some(internal) = self.names.get(internal) else {
            return false;
        };
        self.entry(package)
            .is_some_and(|entry| entry.facades.contains(&internal))
    }
}

/// A node in the composed classpath package table: every jar that declares THIS package (union across the
/// classpath, in declaration order). One jar sits in many package nodes.
#[derive(Default)]
pub struct PackageNode {
    jars: Vec<JarId>,
    /// Entries whose catalog records a `.kotlin_builtins` fragment for this package.
    builtins_jars: Vec<JarId>,
}

#[derive(Default)]
pub struct PackageTree {
    names: NameTree,
    packages: HashMap<NameId, PackageNode>,
    /// Exact class owners, sorted by name and classpath order.
    classes: Vec<(NameId, JarId)>,
    incomplete_entries: Vec<JarId>,
}

impl PackageTree {
    /// Every class the classpath declares, as its slashed internal name and the jar that owns it.
    ///
    /// Sorted by name and classpath order, which is the order the catalog was composed in: the
    /// first jar to declare a name is the one that wins resolution, and iteration preserves that.
    pub fn classes(&self) -> impl Iterator<Item = (String, JarId)> + '_ {
        self.classes
            .iter()
            .map(|(name, jar)| (self.names.render(*name), *jar))
    }

    /// The node for a slashed package path (`""` = this root), or `None` if no jar declares it. The
    /// resolution seam (wired in a later rollout step); exercised now by the compose unit tests.
    #[allow(dead_code)]
    fn node_for(&self, pkg: &str) -> Option<&PackageNode> {
        self.names.get(pkg).and_then(|id| self.packages.get(&id))
    }

    fn jars_for_class(&self, internal: &str) -> Vec<JarId> {
        let Some(class) = self.names.get(internal) else {
            return Vec::new();
        };
        let start = self
            .classes
            .partition_point(|&(candidate, _)| candidate.0 < class.0);
        self.classes[start..]
            .iter()
            .take_while(|&&(candidate, _)| candidate == class)
            .map(|&(_, jar)| jar)
            .collect()
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
    if let Some(internal) = name.strip_suffix(".class") {
        let class = jp.names.insert(internal);
        jp.classes.push(class);
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
        Entry::Jar(j) => jp.complete = build_jar_packages_jar(j, &mut jp),
        Entry::Dir(d) => jp.complete = build_jar_packages_dir(d, d, &mut jp),
        Entry::Jimage(p) => {
            let Some(idx) = cached_jimage_index(p) else {
                return jp;
            };
            for &internal in idx.by_name.keys() {
                let Some(pkg) = idx.names.parent(internal) else {
                    continue;
                };
                let class = jp.names.insert_from(&idx.names, internal);
                jp.classes.push(class);
                if pkg == NameTree::ROOT {
                    continue;
                }
                let pkg = jp.names.insert_from(&idx.names, pkg);
                jp.packages.entry(pkg).or_default().has_classes = true;
            }
            jp.complete = !idx.by_name.is_empty();
        }
    }
    jp
}

fn build_jar_packages_jar(jar: &Path, jp: &mut JarPackages) -> bool {
    let f = match File::open(jar) {
        Ok(file) => file,
        // A missing entry is empty until its snapshot changes.
        Err(error) => return error.kind() == std::io::ErrorKind::NotFound,
    };
    let Ok(mut archive) = zip::ZipArchive::new(f) else {
        return false;
    };
    // Name pass over the central directory — no decompression. Defer reading `kotlin_module` bytes.
    let mut module_indices = Vec::new();
    let mut complete = true;
    for i in 0..archive.len() {
        let Some(name) = archive.name_for_index(i) else {
            complete = false;
            continue;
        };
        if name.starts_with("META-INF/") && name.ends_with(".kotlin_module") {
            module_indices.push(i);
        } else {
            record_pkg_entry_name(name, jp);
        }
    }
    for i in module_indices {
        let Ok(mut e) = archive.by_index(i) else {
            complete = false;
            continue;
        };
        let mut buf = Vec::new();
        if e.read_to_end(&mut buf).is_ok() {
            record_kotlin_module(&buf, jp);
        } else {
            complete = false;
        }
    }
    complete
}

fn build_jar_packages_dir(root: &Path, dir: &Path, jp: &mut JarPackages) -> bool {
    let mut ancestors = HashSet::new();
    build_jar_packages_dir_visited(root, dir, jp, &mut ancestors)
}

fn build_jar_packages_dir_visited(
    root: &Path,
    dir: &Path,
    jp: &mut JarPackages,
    ancestors: &mut HashSet<PathBuf>,
) -> bool {
    let canonical = match enter_directory(dir, ancestors) {
        Ok(Some(canonical)) => canonical,
        // A cycle leaves the catalog incomplete because alias paths remain directly addressable.
        Ok(None) => return false,
        Err(error) => return error.kind() == std::io::ErrorKind::NotFound,
    };
    let rd = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            ancestors.remove(&canonical);
            return error.kind() == std::io::ErrorKind::NotFound;
        }
    };
    let mut complete = true;
    for entry in rd {
        let Ok(e) = entry else {
            complete = false;
            continue;
        };
        let p = e.path();
        if p.is_dir() {
            complete &= build_jar_packages_dir_visited(root, &p, jp, ancestors);
            continue;
        }
        let Ok(rel) = p.strip_prefix(root) else {
            complete = false;
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.ends_with(".kotlin_module") {
            if let Ok(b) = std::fs::read(&p) {
                record_kotlin_module(&b, jp);
            } else {
                complete = false;
            }
        } else {
            record_pkg_entry_name(&rel, jp);
            // A class DIRECTORY often carries no `META-INF/*.kotlin_module` (a jar build writes one;
            // a separate-compilation output dir usually doesn't), which would leave its packages with
            // an empty facade catalog — cross-module top-level/extension resolution silently blind.
            // The catalog is fully recoverable from each class's own `@Metadata`: a file facade/part
            // declares `Package` members, a multifile facade lists its parts. Record those classes as
            // the package's facades, exactly as `kotlin_module` would have.
            if rel.ends_with(".class") {
                if let Some(ci) = std::fs::read(&p).ok().and_then(|b| parse_class(&b).ok()) {
                    let m = &ci.meta;
                    if !m.package_functions.is_empty()
                        || !m.package_properties.is_empty()
                        || !m.type_aliases.is_empty()
                        || !m.multifile_parts.is_empty()
                    {
                        let internal = ci.this_class.render();
                        let pkg = internal.rsplit_once('/').map_or("", |(p, _)| p);
                        let facade_id = jp.names.insert(&internal);
                        let entry = jp.entry_mut(pkg);
                        if !entry.facades.contains(&facade_id) {
                            entry.facades.push(facade_id);
                        }
                    }
                } else {
                    complete = false;
                }
            }
        }
    }
    ancestors.remove(&canonical);
    complete
}

/// Compose per-jar [`JarPackages`] into the merged [`PackageTree`] — a cheap union: every package a jar
/// declares adds that jar to the package's node (in classpath declaration order).
fn compose_package_tree(parts: &[std::sync::Arc<JarPackages>]) -> PackageTree {
    let mut tree = PackageTree::default();
    for (jar_id, jp) in parts.iter().enumerate() {
        if !jp.complete {
            tree.incomplete_entries.push(jar_id);
        }
        for (&pkg_id, entry) in &jp.packages {
            let pkg = tree.names.insert_from(&jp.names, pkg_id);
            let node = tree.packages.entry(pkg).or_default();
            if !node.jars.contains(&jar_id) {
                node.jars.push(jar_id);
            }
            if entry.has_builtins && !node.builtins_jars.contains(&jar_id) {
                node.builtins_jars.push(jar_id);
            }
        }
        for &class_id in &jp.classes {
            let class = tree.names.insert_from(&jp.names, class_id);
            tree.classes.push((class, jar_id));
        }
    }
    tree.classes
        .sort_unstable_by_key(|&(class, jar)| (class.0, jar));
    tree.classes.dedup();
    tree
}

/// The classpath is the JVM realization of the inliner's narrow [`MethodBodies`] capability — the
/// emitter sees only this, not the whole `Classpath`.
impl super::inline::MethodBodies for Classpath {
    fn body(&self, owner: &str, name: &str, descriptor: &str) -> Option<MethodCode> {
        self.method_code(owner, name, descriptor)
    }
    fn owner_is_interface(&self, owner: &str) -> bool {
        // Prefer the real class flag; otherwise the mapped builtin's own `.kotlin_builtins`
        // `CLASS_KIND`. A Kotlin builtin and the JVM class it maps to always agree on interface-ness
        // (`List`/`java.util.List`, `Number`/`java.lang.Number`), so no curated per-name table is
        // needed — the one this replaced omitted every `java/util/*` and answered "class" for them,
        // which emitted `invokevirtual` on an interface whenever no JDK supplied the class file.
        self.find(owner)
            .map(|ci| ci.is_interface())
            .or_else(|| {
                let owner_id = type_name(owner);
                let kotlin =
                    crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(owner_id)
                        .unwrap_or(owner_id);
                self.builtin_is_interface_name(kotlin)
            })
            .unwrap_or(false)
    }
    fn method_is_static(&self, owner: &str, name: &str, descriptor: &str) -> bool {
        self.find(owner).is_some_and(|ci| {
            ci.methods
                .iter()
                .any(|m| m.name == name && m.descriptor == descriptor && m.is_static())
        })
    }
    fn member_is_private(&self, owner: &str, name: &str, descriptor: &str) -> bool {
        self.find(owner).is_some_and(|ci| {
            ci.methods
                .iter()
                .any(|m| m.name == name && m.descriptor == descriptor && m.is_private())
                || ci
                    .fields
                    .iter()
                    .any(|f| f.name == name && f.descriptor == descriptor && f.is_private())
        })
    }
    fn property_read_access(
        &self,
        owner: &str,
        property: &str,
    ) -> Option<super::inline::PropertyAccess> {
        // The class file first — it is authoritative whenever the owner has one. A mapped builtin
        // whose JVM owner is absent (no JDK on the classpath) still has a `.kotlin_builtins`
        // declaration carrying the same accessor name, erased descriptor and interface flag; without
        // this fallback the caller invents a JavaBean getter (`getSize`) off the LOGICAL type.
        inherited_property_access(self, owner, property, class_property_read_access)
            .or_else(|| self.builtin_property_read_access(owner, property))
    }
    /// No builtins fallback, deliberately: `.kotlin_builtins` declares no `var` on a mapped type, so
    /// there is no setter for one to answer with (`MutableMap.MutableEntry` exposes `setValue` as a
    /// FUNCTION, which resolves as an ordinary member call, not a property write).
    fn property_write_access(
        &self,
        owner: &str,
        property: &str,
    ) -> Option<super::inline::PropertyAccess> {
        inherited_property_access(self, owner, property, class_property_write_access)
    }
}

/// The physical method a Kotlin BUILTIN property is realized as on its mapped JVM type — the READ
/// direction of the property-accessor mapping (the WRITE direction is the bridge synthesis in
/// `names::collection_property_stub_name`, reused here): a special `JavaToKotlinClassMap` collection
/// stub (`keys` → `keySet`), the `CharSequence.length` plain method, else the JavaBean getter
/// (`is`-prefix kept, otherwise `get<Name>`).
///
/// One definition shared by the member table (`Classpath::builtin_members_name`) and the realization
/// seam (`Classpath::builtin_property_read_access`), so a call and a property read of the same builtin
/// can never disagree about which method they name.
fn builtin_property_jvm_name(property: &str) -> String {
    if let Some(stub) = crate::jvm::names::collection_property_stub_name(property) {
        stub.to_string()
    } else if property == "length" {
        property.to_string()
    } else {
        crate::jvm::names::property_getter_name(property)
    }
}

/// Resolve one target realization over the owner's supertype closure. Reads and writes must walk the
/// exact same breadth-first order so the nearest declaration wins consistently; keeping that traversal
/// here prevents the two operations from drifting as new classpath shapes are added.
fn inherited_property_access(
    classpath: &Classpath,
    owner: &str,
    property: &str,
    declared_access: fn(&ClassInfo, &str) -> Option<super::inline::PropertyAccess>,
) -> Option<super::inline::PropertyAccess> {
    let mut queue = std::collections::VecDeque::new();
    let mut seen = std::collections::HashSet::new();
    queue.push_back(super::jvm_class_map::to_jvm_type_name(type_name(owner)));
    while let Some(current) = queue.pop_front() {
        if !seen.insert(current) {
            continue;
        }
        let Some(class) = classpath.find_name(current) else {
            continue;
        };
        if let Some(access) = declared_access(&class, property) {
            return Some(access);
        }
        queue.extend(class.super_class);
        queue.extend(class.interfaces.iter_ids());
    }
    None
}

/// The write analogue of [`class_property_read_access`]: the setter `@Metadata` names for `property`, else
/// the bean setter of a Java class, else a public non-final field. `None` for a read-only property.
fn class_property_write_access(
    ci: &ClassInfo,
    property: &str,
) -> Option<super::inline::PropertyAccess> {
    use super::inline::PropertyAccess;
    let owner = ci.this_class().to_string();
    let setter = |method: &super::classreader::MethodSig| PropertyAccess::Accessor {
        owner: owner.clone(),
        name: method.name.clone(),
        descriptor: method.descriptor.clone(),
        is_static: method.is_static(),
        is_interface: ci.is_interface(),
    };
    let one_arg = |name: &str| {
        ci.methods
            .iter()
            .find(|m| {
                m.name == name
                    && super::names::parse_method_descriptor(&m.descriptor)
                        .is_some_and(|(parameters, ret)| parameters.len() == 1 && ret == "V")
            })
            .cloned()
    };
    if let Some(declared) = super::metadata::class_properties(ci)
        .iter()
        .find(|p| p.name == property && !p.is_extension)
    {
        if let Some(method) = declared.setter.as_ref().and_then(|setter| {
            ci.methods
                .iter()
                .find(|m| m.name == setter.name && m.descriptor == setter.desc)
        }) {
            return Some(setter(method));
        }
    } else if let Some(method) = one_arg(&crate::names::property_setter_name(property)) {
        return Some(setter(&method));
    }
    let field = ci.fields.iter().find(|f| {
        f.name == property
            && f.access & super::classreader::ACC_PUBLIC != 0
            && f.access & 0x0010 == 0 // not ACC_FINAL
    })?;
    Some(PropertyAccess::Field {
        owner,
        name: field.name.clone(),
        descriptor: field.descriptor.clone(),
        is_static: field.access & super::classreader::ACC_STATIC != 0,
    })
}

/// The realization of property `property` DECLARED by `ci` itself (no supertype walk). `@Metadata`'s
/// `JvmPropertySignature` names the accessor and/or backing field authoritatively — never a `getX` guess —
/// and the class file's access flags say whether it takes a receiver. An accessor is preferred over a
/// field: a private backing field is unreadable from outside, and a computed property has no field at all.
fn class_property_read_access(
    ci: &ClassInfo,
    property: &str,
) -> Option<super::inline::PropertyAccess> {
    use super::inline::PropertyAccess;
    let owner = ci.this_class().to_string();
    let accessor = |method: &super::classreader::MethodSig| PropertyAccess::Accessor {
        owner: owner.clone(),
        name: method.name.clone(),
        descriptor: method.descriptor.clone(),
        is_static: method.is_static(),
        is_interface: ci.is_interface(),
    };
    let zero_arg = |name: &str| {
        ci.methods
            .iter()
            .find(|m| m.name == name && m.descriptor.starts_with("()") && m.descriptor != "()V")
            .cloned()
    };
    // A Kotlin class: `@Metadata` names the accessor exactly (`@JvmName`, value-class mangling, and the
    // `@JvmStatic` case where it is a static of this class).
    if let Some(declared) = super::metadata::class_properties(ci)
        .iter()
        .find(|p| p.name == property && !p.is_extension)
    {
        if let Some(method) = declared.getter.as_ref().and_then(|getter| {
            ci.methods
                .iter()
                .find(|m| m.name == getter.name && m.descriptor == getter.desc)
        }) {
            return Some(accessor(method));
        }
    } else {
        // A Java class has no property declarations: Kotlin sees a SYNTHETIC property for a `getX()` /
        // `isX()` bean accessor. The Kotlin-name → JVM-name mapping for a mapped builtin (`size` →
        // `size()`, `keys` → `keySet()`) is applied first, since those are not bean-shaped.
        let mapped = super::names::mapped_builtin_virtual_name(&owner, property);
        for candidate in [
            mapped.to_string(),
            crate::names::property_getter_name(property),
            format!("is{}", capitalize(property)),
            // A zero-arg method read under its own name. Kotlin has no synthetic property for this, but
            // krusty's checker admits it, so the realization has to exist or the read would emit nothing.
            property.to_string(),
        ] {
            if let Some(method) = zero_arg(&candidate) {
                return Some(accessor(&method));
            }
        }
    }
    // No accessor method: the property is realized as a plain field (`@JvmField`, a `const val`, or a
    // public Java field surfaced as a Kotlin property).
    let field = ci
        .fields
        .iter()
        .find(|f| f.name == property && f.access & super::classreader::ACC_PUBLIC != 0)?;
    Some(PropertyAccess::Field {
        owner,
        name: field.name.clone(),
        descriptor: field.descriptor.clone(),
        is_static: field.access & super::classreader::ACC_STATIC != 0,
    })
}

fn capitalize(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
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
        .iter()
        .chain(super::metadata::class_functions(&ci).iter())
    {
        if mf.is_extension() {
            ext_names.insert(mf.jvm_name.clone());
        } else {
            toplevel_names.insert(mf.jvm_name.clone());
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
    let mut ancestors = HashSet::new();
    collect_dir_visited(dir, names, all, &mut ancestors);
}

fn collect_dir_visited(
    dir: &Path,
    names: &mut NameTree,
    all: &mut HashMap<NameId, ClassLite>,
    ancestors: &mut HashSet<PathBuf>,
) {
    let Ok(Some(canonical)) = enter_directory(dir, ancestors) else {
        return;
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        ancestors.remove(&canonical);
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_dir_visited(&p, names, all, ancestors);
        } else if p.extension().map_or(false, |x| x == "class") {
            if let Ok(b) = std::fs::read(&p) {
                collect_class_bytes(&b, names, all);
            }
        }
    }
    ancestors.remove(&canonical);
}

fn collect_jar(
    jar: &Path,
    packages: &JarPackages,
    names: &mut NameTree,
    all: &mut HashMap<NameId, ClassLite>,
) {
    let Ok(f) = File::open(jar) else { return };
    let Ok(mut archive) = zip::ZipArchive::new(f) else {
        return;
    };
    for i in 0..archive.len() {
        let wanted = archive
            .name_for_index(i)
            .and_then(class_internal_from_entry)
            .is_some_and(|internal| ext_scan_wanted(internal, packages));
        if !wanted {
            continue;
        }
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
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

fn ext_scan_wanted(internal: &str, packages: &JarPackages) -> bool {
    if packages.contains_facade(internal) {
        return true;
    }
    internal
        .rsplit('/')
        .next()
        .unwrap_or(internal)
        .contains("Kt")
}

fn type_alias_scan_wanted(internal: &str, packages: &JarPackages) -> bool {
    packages.contains_facade(internal) || is_type_aliases_kt(internal)
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
            .insert(type_name(alias), type_name(internal));
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
fn build_entry_types(entry: &Entry, packages: &JarPackages) -> TypeIndex {
    let mut idx = TypeIndex::default();
    match entry {
        Entry::Dir(d) => scan_types_dir(d, &mut idx),
        Entry::Jar(j) => scan_types_jar(j, packages, &mut idx),
        Entry::Jimage(_) => {}
    }
    idx
}

fn build_entry_package_types(
    entry: &Entry,
    packages: &JarPackages,
    package: TypeName,
) -> TypeIndex {
    let mut index = TypeIndex::default();
    let package = package.render();
    match entry {
        Entry::Dir(directory) => {
            if let Some(entry) = packages.entry(&package) {
                for &facade in &entry.facades {
                    let path = directory.join(format!("{}.class", packages.names.render(facade)));
                    if let Ok(bytes) = std::fs::read(path) {
                        parse_aliases_from_bytes(&bytes, &mut index);
                    }
                }
            }
        }
        Entry::Jar(jar) => scan_types_jar_package(jar, packages, &package, &mut index),
        Entry::Jimage(_) => {}
    }
    index
}

fn scan_types_dir(dir: &Path, idx: &mut TypeIndex) {
    let mut ancestors = HashSet::new();
    scan_types_dir_rooted(dir, dir, idx, &mut ancestors);
}

/// Walk `dir` for `*TypeAliasesKt.class` files and decode their Kotlin type aliases. Other classes are
/// skipped — the classpath no longer builds a name → internal map (it was dead; import-driven resolution
/// goes through `resolve_type` / the ext index).
fn scan_types_dir_rooted(
    root: &Path,
    dir: &Path,
    idx: &mut TypeIndex,
    ancestors: &mut HashSet<PathBuf>,
) {
    let Ok(Some(canonical)) = enter_directory(dir, ancestors) else {
        return;
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        ancestors.remove(&canonical);
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            scan_types_dir_rooted(root, &p, idx, ancestors);
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
    ancestors.remove(&canonical);
}

fn scan_types_jar(jar: &Path, packages: &JarPackages, idx: &mut TypeIndex) {
    let Ok(f) = File::open(jar) else { return };
    let Ok(mut archive) = zip::ZipArchive::new(f) else {
        return;
    };
    for i in 0..archive.len() {
        let wanted = archive
            .name_for_index(i)
            .and_then(class_internal_from_entry)
            .is_some_and(|internal| type_alias_scan_wanted(internal, packages));
        if !wanted {
            continue;
        }
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
        let mut buf = Vec::new();
        if entry.read_to_end(&mut buf).is_ok() {
            parse_aliases_from_bytes(&buf, idx);
        }
    }
}

fn scan_types_jar_package(
    jar: &Path,
    packages: &JarPackages,
    package: &str,
    index: &mut TypeIndex,
) {
    let Ok(file) = File::open(jar) else { return };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return;
    };
    for entry_id in 0..archive.len() {
        let wanted = archive
            .name_for_index(entry_id)
            .and_then(class_internal_from_entry)
            .is_some_and(|internal| {
                internal.rsplit_once('/').map_or("", |(parent, _)| parent) == package
                    && type_alias_scan_wanted(internal, packages)
            });
        if !wanted {
            continue;
        }
        let Ok(mut entry) = archive.by_index(entry_id) else {
            continue;
        };
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_ok() {
            parse_aliases_from_bytes(&bytes, index);
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
    fn builtin_type_parameter_erasure_follows_its_primary_bound() {
        let bounded = Ty::ty_param("T", Ty::obj("kotlin/CharSequence"));
        assert_eq!(
            builtin_erased(bounded),
            Ty::obj("kotlin/CharSequence"),
            "a decoded builtins signature must use the same bound erasure as JVM descriptors"
        );
        let unbounded = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        assert_eq!(builtin_erased(unbounded), Ty::obj("kotlin/Any"));
    }

    #[test]
    fn member_metadata_matches_the_jvm_descriptor() {
        use crate::jvm::metadata::{MetaFn, MetaValueParam, MfnFlags, MvpFlags};
        use crate::libraries::GenericSig;
        use crate::types::Visibility;

        let function = |param: Ty, ret: Ty, has_default: bool, suspend: bool| MetaFn {
            kotlin_name: "emit".to_string(),
            jvm_name: "emit".to_string(),
            jvm_desc: None,
            visibility: Visibility::Public,
            flags: MfnFlags::default().with_is_suspend(suspend),
            receiver_class: None,
            ret_class: ret.obj_internal(),
            value_params: vec![MetaValueParam {
                ty: param.obj_internal(),
                name: "value".to_string(),
                flags: MvpFlags::default()
                    .with_has_default(has_default)
                    .with_nullable(param.is_nullable()),
                recv_fun_receiver: None,
            }],
            generic_sig: Some(GenericSig {
                formals: Vec::new(),
                formal_bounds: Vec::new(),
                receiver: None,
                params: vec![param],
                ret,
            }),
            contract: None,
            context_count: 0,
            context_params_nullable: Vec::new(),
        };

        let narrow = [
            function(Ty::Byte, Ty::Unit, true, false),
            function(Ty::Int, Ty::Unit, false, false),
        ];
        let byte =
            aligned_member_metadata(&narrow, "emit", "(B)V", &|_| None).expect("Byte overload");
        assert!(byte.member_call_sig().param_defaults[0]);
        let int =
            aligned_member_metadata(&narrow, "emit", "(I)V", &|_| None).expect("Int overload");
        assert_eq!(int.member_call_sig().required, 1);

        let source = function(Ty::Int, Ty::String, false, false);
        assert!(aligned_member_metadata(&[source], "emit", "(I)V", &|_| None).is_none());

        let bounded = function(
            Ty::ty_param("T", Ty::obj("kotlin/CharSequence")),
            Ty::Unit,
            true,
            false,
        );
        assert!(aligned_member_metadata(
            &[bounded],
            "emit",
            "(Ljava/lang/CharSequence;)V",
            &|_| None
        )
        .is_some());

        let nested = function(Ty::obj("fixture/Outer.Inner"), Ty::Unit, true, false);
        assert!(
            aligned_member_metadata(&[nested], "emit", "(Lfixture/Outer$Inner;)V", &|_| None)
                .is_some()
        );

        let suspended = function(Ty::Byte, Ty::String, true, true);
        assert!(aligned_member_metadata(
            &[suspended],
            "emit",
            "(BLkotlin/coroutines/Continuation;)Ljava/lang/Object;",
            &|_| None,
        )
        .is_some());

        let mut value_class = function(
            Ty::obj("fixture/Token"),
            Ty::obj("fixture/Token"),
            false,
            false,
        );
        value_class.jvm_desc = Some("(Ljava/lang/String;)Ljava/lang/String;");
        assert!(aligned_member_metadata(
            &[value_class],
            "emit",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &|_| None,
        )
        .is_some());

        let nullable_value_class = function(
            Ty::nullable(Ty::obj("fixture/ScalarValue")),
            Ty::Unit,
            false,
            false,
        );
        let scalar_underlying =
            |name: TypeName| name.matches("fixture/ScalarValue").then_some(Ty::Long);
        assert!(aligned_member_metadata(
            &[nullable_value_class],
            "emit",
            "(J)V",
            &scalar_underlying,
        )
        .is_none());

        let mut generic = function(Ty::obj("kotlin/Any"), Ty::String, true, false);
        generic.generic_sig = None;
        generic.value_params[0].ty = None;
        assert!(aligned_member_metadata(
            &[generic.clone()],
            "emit",
            "(Ljava/lang/CharSequence;)Ljava/lang/String;",
            &|_| None,
        )
        .is_some());
        assert!(aligned_member_metadata(
            &[generic.clone(), generic],
            "emit",
            "(Ljava/lang/CharSequence;)Ljava/lang/String;",
            &|_| None,
        )
        .is_none());
    }

    #[test]
    fn package_catalog_selects_jvmname_facades_for_entry_indexes() {
        let module = crate::metadata::module::build_kotlin_module(&[(
            "p".to_string(),
            vec!["Utils".to_string(), "HelpersKt".to_string()],
        )]);
        let mut packages = JarPackages::default();
        record_kotlin_module(&module, &mut packages);

        assert!(packages.contains_facade("p/Utils"));
        assert!(ext_scan_wanted("p/Utils", &packages));
        assert!(type_alias_scan_wanted("p/Utils", &packages));
        assert!(ext_scan_wanted("p/HelpersKt", &packages));
        assert!(!ext_scan_wanted("p/Regular", &packages));

        let empty = JarPackages::default();
        assert!(ext_scan_wanted("p/HelpersKt", &empty));
        assert!(!ext_scan_wanted("p/Regular", &empty));
    }

    #[test]
    fn name_tree_shares_segments_and_renders_internal_names() {
        let tree = NameTree::default();
        let collections = tree.insert("kotlin/collections/CollectionsKt");
        let maps = tree.insert("kotlin/collections/MapsKt");
        let duplicate = tree.insert("kotlin/collections/CollectionsKt");

        assert_eq!(collections, duplicate);
        assert_eq!(tree.render(collections), "kotlin/collections/CollectionsKt");
        assert_eq!(tree.render(maps), "kotlin/collections/MapsKt");
        assert_eq!(tree.len(), 5);
    }

    // `toplevel_only` is a WHOLE-classpath decision evaluated per queried name: top-level in some
    // entry and an extension in none. A name one entry marks top-level and another marks extension
    // must stay receiver-reachable.
    #[test]
    fn toplevel_only_unions_across_entries() {
        let cp = Classpath::new(vec![]);
        let mut a = EntryExt::default();
        a.toplevel_names.insert("run".into());
        let mut b = EntryExt::default();
        b.ext_names.insert("run".into());
        b.toplevel_names.insert("println".into());
        *cp.ext.borrow_mut() = Some(std::rc::Rc::new(vec![
            std::sync::Arc::new(a),
            std::sync::Arc::new(b),
        ]));
        assert!(!cp.ext_toplevel_only("run"));
        assert!(cp.ext_toplevel_only("println"));
        assert!(!cp.ext_toplevel_only("absent"));
    }

    // `find_extension_owners` unions per-entry receiver records in entry order, dedups owners, and
    // drops genuine top-level names.
    #[test]
    fn extension_owners_union_per_entry_records() {
        let cp = Classpath::new(vec![]);
        let mut a = EntryExt::default();
        let a_owner = a.owner_names.insert("kotlin/collections/CollectionsKt");
        a.by_recv_raw.insert(
            "Ljava/lang/Iterable;".to_string(),
            vec![
                ("map".to_string(), a_owner),
                ("filter".to_string(), a_owner),
            ],
        );
        let mut b = EntryExt::default();
        let b_owner = b.owner_names.insert("demo/DemoKt");
        let b_top = b.owner_names.insert("demo/TopKt");
        b.by_recv_raw.insert(
            "Ljava/lang/Iterable;".to_string(),
            vec![("map".to_string(), b_owner), ("runAll".to_string(), b_top)],
        );
        b.toplevel_names.insert("runAll".to_string());
        *cp.ext.borrow_mut() = Some(std::rc::Rc::new(vec![
            std::sync::Arc::new(a),
            std::sync::Arc::new(b),
        ]));
        let owners = cp.find_extension_owners("Ljava/lang/Iterable;");
        assert_eq!(
            owners.iter().map(|o| o.render()).collect::<Vec<_>>(),
            vec!["kotlin/collections/CollectionsKt", "demo/DemoKt"]
        );
        assert!(cp.find_extension_owners("Lother;").is_empty());
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
    fn source_analysis_preparation_builds_classpath_indexes() {
        let classpath = Classpath::empty();
        assert!(classpath.jimage.borrow().is_none());
        assert!(classpath.pkg_tree.borrow().is_none());
        assert!(classpath.types.borrow().is_none());
        assert!(classpath.ext.borrow().is_none());

        classpath.prepare_for_source_analysis();

        assert!(classpath.jimage.borrow().is_some());
        assert!(classpath.pkg_tree.borrow().is_some());
        assert!(classpath.ext.borrow().is_some());
        assert!(classpath.types.borrow().is_none());
        assert!(classpath.aliases.borrow().is_empty());
    }

    #[test]
    fn metadata_param_matching_keeps_unsigned_descriptor_erasure() {
        let uint = type_name("kotlin/UInt");
        let ulong = type_name("kotlin/ULong");
        let no_value_classes = &|_| None;
        assert!(meta_param_compat(
            Some(uint),
            false,
            &Ty::Int,
            no_value_classes
        ));
        assert!(meta_param_compat(
            Some(ulong),
            false,
            &Ty::Long,
            no_value_classes
        ));
        assert!(meta_param_exact(
            Some(uint),
            false,
            &Ty::Int,
            no_value_classes
        ));
        assert!(meta_param_exact(
            Some(ulong),
            false,
            &Ty::Long,
            no_value_classes
        ));

        assert!(!meta_param_compat(
            Some(uint),
            false,
            &Ty::Long,
            no_value_classes
        ));
        assert!(!meta_param_compat(
            Some(ulong),
            false,
            &Ty::Int,
            no_value_classes
        ));
        assert!(!meta_param_exact(
            Some(uint),
            false,
            &Ty::Long,
            no_value_classes
        ));
        assert!(!meta_param_exact(
            Some(ulong),
            false,
            &Ty::Int,
            no_value_classes
        ));
        assert!(!meta_param_compat(
            Some(uint),
            true,
            &Ty::Int,
            no_value_classes
        ));
    }

    #[test]
    fn metadata_param_matching_erases_value_classes_to_their_underlying() {
        // A value class over `Long` has a primitive JVM descriptor while metadata names the class.
        // Non-null alignment sees through erasure; nullable alignment must retain the boxed class.
        let scalar_value = type_name("fixture/ScalarValue");
        let vc = &|name: TypeName| (name == scalar_value).then_some(Ty::Long);
        assert!(meta_param_compat(Some(scalar_value), false, &Ty::Long, vc));
        assert!(meta_param_exact(Some(scalar_value), false, &Ty::Long, vc));
        assert!(!meta_param_compat(Some(scalar_value), false, &Ty::Int, vc));
        assert!(!meta_param_exact(Some(scalar_value), false, &Ty::Int, vc));
        assert!(!meta_param_compat(Some(scalar_value), true, &Ty::Long, vc));
        // Without value-class knowledge the class name does not match the primitive (the old
        // behavior that dropped `runTest`'s metadata alignment).
        let no_value_classes = &|_| None;
        assert!(!meta_param_compat(
            Some(scalar_value),
            false,
            &Ty::Long,
            no_value_classes
        ));

        // A value class over an UNSIGNED primitive erases to the signed carrier (`UInt` → `I`),
        // normalizing exactly like the mapped builtins above.
        let id = type_name("sample/Id");
        let vc_uint = &|name: TypeName| (name == id).then_some(Ty::UInt);
        assert!(meta_param_compat(Some(id), false, &Ty::Int, vc_uint));
        assert!(meta_param_exact(Some(id), false, &Ty::Int, vc_uint));
        assert!(!meta_param_compat(Some(id), false, &Ty::Long, vc_uint));
    }

    /// The provisioned kotlin-stdlib jar via the project's single CI-safe resolver
    /// ([`crate::toolchain::stdlib_jar`] — the dist env vars `KRUSTY_KOTLINC`/`KRUSTY_KOTLIN_STDLIB`, then
    /// the gradle/m2 caches). A test returns early when it is absent (toolchain not provisioned), so it
    /// never fails on CI regardless of where the stdlib lives.
    fn test_stdlib_jar() -> Option<PathBuf> {
        crate::toolchain::stdlib_jar()
    }

    #[test]
    fn alias_lookup_scopes_to_the_declaring_package_without_composing_every_entry() {
        let Some(jar) = test_stdlib_jar() else {
            return;
        };
        let cp = Classpath::new(vec![jar]);

        let target = cp.type_alias_target_name(type_name("kotlin/collections/ArrayList"));
        assert!(
            target.is_some_and(|target| target.matches("java/util/ArrayList")),
            "classpath typealias still resolves without the eager whole-classpath scan"
        );
        assert!(cp.types.borrow().is_none());
        let package = type_name("kotlin/collections");
        assert!(cp.aliases.borrow_mut().get(&package).is_some_and(|index| {
            index
                .type_aliases
                .keys()
                .all(|alias| alias.parent() == Some(package))
        }));
        assert!(cp
            .type_alias_target_name(type_name("kotlin/collections/CollectionsKt"))
            .is_none());
    }

    #[test]
    fn lazy_alias_merge_preserves_classpath_order() {
        let alias = type_name("sample/Alias");
        let earlier_target = type_name("sample/Earlier");
        let later_target = type_name("sample/Later");
        let mut earlier = TypeIndex::default();
        earlier.type_aliases.insert(alias, earlier_target);
        let mut later = TypeIndex::default();
        later.type_aliases.insert(alias, later_target);

        let mut aliases = TypeIndex::default();
        merge_alias_part(&mut aliases, &earlier);
        merge_alias_part(&mut aliases, &later);

        assert_eq!(aliases.type_aliases.get(&alias), Some(&earlier_target));
    }

    #[test]
    fn incomplete_catalogs_do_not_enter_the_lazy_alias_cache() {
        let directory = test_temp_dir("incomplete-alias-catalog");
        let jar = directory.join("broken.jar");
        std::fs::write(&jar, b"not a zip").expect("write broken jar");
        let cp = Classpath::new(vec![jar]);

        assert!(!cp.package_tree().incomplete_entries.is_empty());
        assert!(cp
            .type_alias_target_name(type_name("sample/Missing"))
            .is_none());
        assert!(cp.aliases.borrow().is_empty());
        assert!(cp.types.borrow().is_none());

        drop(cp);
        std::fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn alias_package_misses_are_bounded() {
        let cp = Classpath::empty();
        for index in 0..=ALIAS_PACKAGE_CAP {
            cp.type_alias_target_name(type_name(&format!("missing{index}/Alias")));
        }

        assert_eq!(cp.aliases.borrow().len(), ALIAS_PACKAGE_CAP);
        assert!(cp.cache_report().contains("alias_pkg=1024"));
    }

    #[test]
    fn owning_jar_returns_the_jar_path_for_a_library_class() {
        let Some(jar) = test_stdlib_jar() else {
            return; // toolchain not provisioned
        };
        let cp = Classpath::new(vec![jar.clone()]);
        let owner = cp.owning_jar("kotlin/collections/CollectionsKt");
        assert_eq!(owner.as_deref(), Some(jar.as_path()));
    }

    fn write_test_jar_entry(path: &Path, name: &str, contents: &[u8]) {
        use std::io::Write;

        let file = File::create(path).expect("create test jar");
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .expect("start test jar entry");
        archive.write_all(contents).expect("write test jar entry");
        archive.finish().expect("finish test jar");
    }

    fn write_test_jar(path: &Path, contents: &[u8]) {
        write_test_jar_entry(path, "sample.txt", contents);
    }

    fn test_temp_dir(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("krusty-{tag}-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).expect("create test directory");
        directory
    }

    #[test]
    fn open_jar_cache_is_bounded_and_skips_unknown_packages() {
        let directory = test_temp_dir("open-jar-cache");
        let mut paths = Vec::new();
        for index in 0..(OPEN_ARCHIVE_CAP * 4) {
            let path = directory.join(format!("{index}.jar"));
            write_test_jar(&path, b"entry");
            paths.push(path);
        }
        let unresolved = Classpath::new(paths.clone());
        assert!(unresolved.find("late/Added").is_none());
        assert!(unresolved.archives.borrow().is_empty());
        assert!(unresolved.snapshot_is_current());
        let added =
            crate::jvm::classfile::ClassWriter::new("late/Added", "java/lang/Object").finish();
        write_test_jar_entry(&paths[0], "late/Added.class", &added);
        assert!(!unresolved.snapshot_is_current());

        let classpath = Classpath::new(Vec::new());
        for path in paths.iter().skip(1) {
            assert_eq!(
                classpath.jar_entry(path, "sample.txt").as_deref(),
                Some(b"entry".as_slice())
            );
            assert!(classpath.archives.borrow().len() <= OPEN_ARCHIVE_CAP);
        }
        drop(classpath);
        drop(unresolved);
        for path in paths {
            std::fs::remove_file(path).expect("archive closes when evicted or dropped");
        }
        std::fs::remove_dir(directory).expect("remove jar cache directory");
    }

    #[test]
    fn builtin_miss_does_not_open_unrelated_archives() {
        let directory = test_temp_dir("builtin-miss");
        let mut paths = Vec::new();
        for index in 0..(OPEN_ARCHIVE_CAP * 4) {
            let path = directory.join(format!("{index}.jar"));
            write_test_jar(&path, b"entry");
            paths.push(path);
        }
        let classpath = Classpath::new(paths.clone());

        let builtins = classpath.builtins_file_for_package(type_name("unrelated/package"));

        assert!(builtins.classes.is_empty());
        assert!(classpath.archives.borrow().is_empty());
        drop(builtins);
        drop(classpath);
        for path in paths {
            std::fs::remove_file(path).expect("archive remains closed");
        }
        std::fs::remove_dir(directory).expect("remove builtin miss directory");
    }

    #[test]
    fn exact_class_miss_does_not_open_same_package_archives() {
        let directory = test_temp_dir("exact-class-miss");
        let mut paths = Vec::new();
        for index in 0..(OPEN_ARCHIVE_CAP * 4) {
            let path = directory.join(format!("{index}.jar"));
            write_test_jar_entry(
                &path,
                &format!("shared/package/Present{index}.class"),
                b"class bytes are read lazily",
            );
            paths.push(path);
        }
        let classpath = Classpath::new(paths.clone());

        assert!(classpath.find("shared/package/Missing").is_none());
        assert!(classpath.archives.borrow().is_empty());
        drop(classpath);
        for path in paths {
            std::fs::remove_file(path).expect("archive remains closed");
        }
        std::fs::remove_dir(directory).expect("remove exact class miss directory");
    }

    #[test]
    fn raw_class_miss_does_not_open_same_package_archives() {
        let directory = test_temp_dir("raw-class-miss");
        let mut paths = Vec::new();
        for index in 0..(OPEN_ARCHIVE_CAP * 4) {
            let path = directory.join(format!("{index}.jar"));
            write_test_jar_entry(
                &path,
                &format!("shared/package/Present{index}.class"),
                b"class bytes are read lazily",
            );
            paths.push(path);
        }
        let classpath = Classpath::new(paths.clone());

        assert!(classpath.class_bytes("shared/package/Missing").is_none());
        assert!(classpath.archives.borrow().is_empty());
        drop(classpath);
        for path in paths {
            std::fs::remove_file(path).expect("archive remains closed");
        }
        std::fs::remove_dir(directory).expect("remove raw class miss directory");
    }

    #[test]
    fn warmed_directory_catalog_detects_a_generated_package() {
        let directory = test_temp_dir("live-class-dir");
        let classpath = Classpath::new(vec![directory.clone()]);
        assert!(classpath.package_tree().node_for("generated").is_none());
        assert!(classpath.find("generated/Later").is_none());
        assert!(classpath.snapshot_is_current());

        let package = directory.join("generated");
        std::fs::create_dir(&package).expect("create generated package");
        let bytes =
            crate::jvm::classfile::ClassWriter::new("generated/Later", "java/lang/Object").finish();
        std::fs::write(package.join("Later.class"), bytes).expect("write generated class");

        assert!(!classpath.snapshot_is_current());
        let refreshed = Classpath::new(vec![directory.clone()]);
        assert!(refreshed.find("generated/Later").is_some());
        drop(refreshed);
        drop(classpath);
        std::fs::remove_dir_all(&directory).expect("remove class directory");
    }

    #[test]
    fn missing_output_is_an_authoritative_empty_snapshot() {
        let parent = test_temp_dir("missing-class-output");
        let output = parent.join("not-built-yet");
        let classpath = Classpath::new(vec![output.clone()]);

        assert!(classpath.package_tree().incomplete_entries.is_empty());
        assert!(classpath.find("generated/Later").is_none());
        assert!(classpath.snapshot_is_current());

        let package = output.join("generated");
        std::fs::create_dir_all(&package).expect("create generated package");
        let bytes =
            crate::jvm::classfile::ClassWriter::new("generated/Later", "java/lang/Object").finish();
        std::fs::write(package.join("Later.class"), bytes).expect("write generated class");
        assert!(!classpath.snapshot_is_current());
        assert!(classpath.find("generated/Later").is_none());

        let refreshed = Classpath::new(vec![output]);
        assert!(refreshed.find("generated/Later").is_some());

        drop(refreshed);
        drop(classpath);
        std::fs::remove_dir_all(parent).expect("remove class output parent");
    }

    #[test]
    fn incomplete_entry_retries_a_negative_class_probe() {
        let directory = test_temp_dir("incomplete-entry-recovery");
        let package = directory.join("recovered");
        std::fs::create_dir(&package).expect("create package");
        let class_file = package.join("Later.class");
        let valid =
            crate::jvm::classfile::ClassWriter::new("recovered/Later", "java/lang/Object").finish();
        let mut invalid = valid.clone();
        invalid[0] = 0;
        std::fs::write(&class_file, invalid).expect("write temporarily unreadable class");

        let classpath = Classpath::new(vec![directory.clone()]);
        assert!(classpath.find("recovered/Later").is_none());
        std::fs::write(&class_file, valid).expect("recover class");
        assert!(classpath.find("recovered/Later").is_some());

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove class directory");
    }

    #[test]
    fn incomplete_entry_reloads_a_positive_class_probe() {
        let directory = test_temp_dir("incomplete-positive-recovery");
        let broken_package = directory.join("broken");
        let changed_package = directory.join("changed");
        std::fs::create_dir(&broken_package).expect("create broken package");
        std::fs::create_dir(&changed_package).expect("create changed package");
        std::fs::write(broken_package.join("Broken.class"), b"not a class")
            .expect("write malformed class");
        let class_file = changed_package.join("Value.class");
        let original =
            crate::jvm::classfile::ClassWriter::new("changed/Value", "java/lang/Object").finish();
        std::fs::write(&class_file, original).expect("write original class");

        let classpath = Classpath::new(vec![directory.clone()]);
        assert_eq!(
            classpath
                .find("changed/Value")
                .and_then(|class| class.super_class()),
            Some("java/lang/Object".to_string())
        );
        let replacement =
            crate::jvm::classfile::ClassWriter::new("changed/Value", "java/lang/Number").finish();
        std::fs::write(&class_file, replacement).expect("write replacement class");
        assert_eq!(
            classpath
                .find("changed/Value")
                .and_then(|class| class.super_class()),
            Some("java/lang/Number".to_string())
        );

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove class directory");
    }

    #[test]
    fn incomplete_entry_does_not_cache_a_later_shadow() {
        let directory = test_temp_dir("incomplete-shadow");
        let earlier = directory.join("earlier/shadow");
        let later = directory.join("later/shadow");
        std::fs::create_dir_all(&earlier).expect("create earlier package");
        std::fs::create_dir_all(&later).expect("create later package");
        let earlier_file = earlier.join("Chosen.class");
        let recovered =
            crate::jvm::classfile::ClassWriter::new("shadow/Chosen", "java/lang/Number").finish();
        let mut invalid = recovered.clone();
        invalid[0] = 0;
        std::fs::write(&earlier_file, invalid).expect("write incomplete earlier class");
        let fallback =
            crate::jvm::classfile::ClassWriter::new("shadow/Chosen", "java/lang/Object").finish();
        std::fs::write(later.join("Chosen.class"), fallback).expect("write later class");

        let classpath = Classpath::new(vec![directory.join("earlier"), directory.join("later")]);
        assert_eq!(
            classpath
                .find("shadow/Chosen")
                .and_then(|ci| ci.super_class()),
            Some("java/lang/Object".to_string())
        );
        std::fs::write(&earlier_file, recovered).expect("recover earlier class");
        assert_eq!(
            classpath
                .find("shadow/Chosen")
                .and_then(|ci| ci.super_class()),
            Some("java/lang/Number".to_string())
        );

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove class directory");
    }

    #[test]
    fn semantic_resolution_retries_an_incomplete_entry() {
        use crate::symbol_source::SymbolSource;

        let directory = test_temp_dir("incomplete-semantic-recovery");
        let package = directory.join("recovered");
        std::fs::create_dir(&package).expect("create package");
        let class_file = package.join("Later.class");
        let valid =
            crate::jvm::classfile::ClassWriter::new("recovered/Later", "java/lang/Object").finish();
        let mut invalid = valid.clone();
        invalid[0] = 0;
        std::fs::write(&class_file, invalid).expect("write incomplete class");
        let classpath = std::rc::Rc::new(Classpath::new(vec![directory.clone()]));
        let libraries = crate::jvm::jvm_libraries::JvmLibraries::new(classpath.clone());

        assert!(libraries
            .resolve_symbols("recovered/Later")
            .classifier
            .is_none());
        std::fs::write(&class_file, valid).expect("recover class");
        assert!(libraries
            .resolve_symbols("recovered/Later")
            .classifier
            .is_some());

        drop(libraries);
        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove class directory");
    }

    #[test]
    fn classpath_snapshot_detects_a_nested_class_overwrite() {
        use std::io::Write;

        let directory = test_temp_dir("nested-class-revision");
        let package = directory.join("existing/package");
        std::fs::create_dir_all(&package).expect("create existing package");
        let class_file = package.join("Changed.class");
        let bytes =
            crate::jvm::classfile::ClassWriter::new("existing/package/Changed", "java/lang/Object")
                .finish();
        std::fs::write(&class_file, bytes).expect("write initial class");
        let classpath = Classpath::new(vec![directory.clone()]);
        assert!(classpath.snapshot_is_current());

        std::fs::OpenOptions::new()
            .append(true)
            .open(&class_file)
            .expect("open nested class")
            .write_all(&[0])
            .expect("overwrite nested class");

        assert!(!classpath.snapshot_is_current());
        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove class directory");
    }

    #[cfg(unix)]
    #[test]
    fn classpath_snapshot_follows_symlinks_without_cycles() {
        use std::io::Write;
        use std::os::unix::fs::symlink;

        let directory = test_temp_dir("symlink-class-revision");
        let targets = test_temp_dir("symlink-class-targets");
        let linked_file = targets.join("Linked.class");
        let linked_bytes =
            crate::jvm::classfile::ClassWriter::new("Linked", "java/lang/Object").finish();
        std::fs::write(&linked_file, linked_bytes).expect("write linked class");
        symlink(&linked_file, directory.join("Linked.class")).expect("link class file");

        let linked_directory = targets.join("package");
        std::fs::create_dir(&linked_directory).expect("create linked package");
        let nested_file = linked_directory.join("Nested.class");
        let nested_bytes =
            crate::jvm::classfile::ClassWriter::new("package/Nested", "java/lang/Object").finish();
        std::fs::write(&nested_file, nested_bytes).expect("write nested linked class");
        symlink(&linked_directory, directory.join("package")).expect("link package directory");
        symlink(&directory, directory.join("loop")).expect("link directory cycle");

        let file_snapshot = Classpath::new(vec![directory.clone()]);
        file_snapshot.prepare_for_source_analysis();
        assert!(file_snapshot.snapshot_is_current());
        std::fs::OpenOptions::new()
            .append(true)
            .open(&linked_file)
            .expect("open linked class target")
            .write_all(b"-changed")
            .expect("change linked class target");
        assert!(!file_snapshot.snapshot_is_current());

        let directory_snapshot = Classpath::new(vec![directory.clone()]);
        assert!(directory_snapshot.snapshot_is_current());
        std::fs::OpenOptions::new()
            .append(true)
            .open(&nested_file)
            .expect("open linked directory target")
            .write_all(b"-changed")
            .expect("change linked directory target");
        assert!(!directory_snapshot.snapshot_is_current());

        drop(directory_snapshot);
        drop(file_snapshot);
        std::fs::remove_dir_all(directory).expect("remove symlink directory");
        std::fs::remove_dir_all(targets).expect("remove symlink targets");
    }

    #[cfg(unix)]
    #[test]
    fn classpath_catalogs_each_symlink_alias() {
        use std::os::unix::fs::symlink;

        let directory = test_temp_dir("symlink-aliases");
        let target = test_temp_dir("symlink-alias-target");
        let left_package = target.join("left");
        let right_package = target.join("right");
        std::fs::create_dir(&left_package).expect("create left package");
        std::fs::create_dir(&right_package).expect("create right package");
        let through_left =
            crate::jvm::classfile::ClassWriter::new("left/right/Shared", "java/lang/Object")
                .finish();
        let through_right =
            crate::jvm::classfile::ClassWriter::new("right/left/Shared", "java/lang/Object")
                .finish();
        std::fs::write(right_package.join("Shared.class"), through_left)
            .expect("write class reached through left alias");
        std::fs::write(left_package.join("Shared.class"), through_right)
            .expect("write class reached through right alias");
        symlink(&target, directory.join("left")).expect("create left alias");
        symlink(&target, directory.join("right")).expect("create right alias");

        let classpath = Classpath::new(vec![directory.clone()]);
        assert!(classpath.find("left/right/Shared").is_some());
        assert!(classpath.find("right/left/Shared").is_some());

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove alias directory");
        std::fs::remove_dir_all(target).expect("remove alias target");
    }

    #[cfg(unix)]
    #[test]
    fn classpath_snapshot_detects_symlink_retargeting() {
        use std::os::unix::fs::symlink;

        let directory = test_temp_dir("symlink-retarget");
        let targets = test_temp_dir("symlink-retarget-targets");
        let first = targets.join("first.class");
        let second = targets.join("second.class");
        let bytes = crate::jvm::classfile::ClassWriter::new("Linked", "java/lang/Object").finish();
        std::fs::write(&first, bytes).expect("write first target");
        std::fs::hard_link(&first, &second).expect("create equal-metadata target");
        let linked = directory.join("Linked.class");
        symlink(&first, &linked).expect("create initial symlink");

        let classpath = Classpath::new(vec![directory.clone()]);
        assert!(classpath.snapshot_is_current());
        std::fs::remove_file(&linked).expect("remove initial symlink");
        symlink(&second, &linked).expect("retarget symlink");
        assert!(!classpath.snapshot_is_current());

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove symlink directory");
        std::fs::remove_dir_all(targets).expect("remove symlink targets");
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_does_not_hide_a_class_change() {
        use std::io::Write;
        use std::os::unix::fs::symlink;

        let directory = test_temp_dir("broken-symlink-revision");
        symlink(
            directory.join("permanently-missing"),
            directory.join("broken"),
        )
        .expect("create broken symlink");
        let package = directory.join("existing");
        std::fs::create_dir(&package).expect("create package");
        let class_file = package.join("Changed.class");
        let bytes = crate::jvm::classfile::ClassWriter::new("existing/Changed", "java/lang/Object")
            .finish();
        std::fs::write(&class_file, bytes).expect("write class");

        let classpath = Classpath::new(vec![directory.clone()]);
        assert!(classpath.snapshot_is_current());
        std::fs::OpenOptions::new()
            .append(true)
            .open(&class_file)
            .expect("open class")
            .write_all(&[0])
            .expect("change class");
        assert!(!classpath.snapshot_is_current());

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove class directory");
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

    // The parsed-class L2 is keyed per ENTRY, not per classpath set: two classpaths that differ only
    // by an extra entry (a per-test module output dir) share the stdlib jar's cache, so its classes
    // are parsed once per process — not once per distinct classpath vector.
    #[test]
    fn entry_class_caches_are_shared_across_classpath_sets() {
        let shared = PathBuf::from("/nonexistent/shared.jar");
        let a = Classpath::new(vec![shared.clone()]);
        let b = Classpath::new(vec![
            PathBuf::from("/nonexistent/module-out"),
            shared.clone(),
        ]);
        assert!(std::sync::Arc::ptr_eq(
            &a.entry_caches[0],
            &b.entry_caches[1]
        ));
        assert!(!std::sync::Arc::ptr_eq(
            &a.entry_caches[0],
            &b.entry_caches[0]
        ));
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
        assert_eq!(
            tree.node_for("kotlin/collections").unwrap().builtins_jars,
            vec![1]
        );
        assert!(tree.node_for("kotlin").unwrap().builtins_jars.is_empty());
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
        assert_eq!(
            jp.classes
                .iter()
                .map(|&class| jp.names.render(class))
                .collect::<Vec<_>>(),
            vec!["kotlin/collections/CollectionsKt", "Top"]
        );
    }

    #[test]
    fn compose_routes_exact_classes_in_classpath_order() {
        let mut first = JarPackages::default();
        record_pkg_entry_name("shared/One.class", &mut first);
        record_pkg_entry_name("shared/Duplicate.class", &mut first);
        let mut second = JarPackages::default();
        record_pkg_entry_name("shared/Two.class", &mut second);
        record_pkg_entry_name("shared/Duplicate.class", &mut second);

        let tree = compose_package_tree(&[std::sync::Arc::new(first), std::sync::Arc::new(second)]);

        assert_eq!(tree.jars_for_class("shared/One"), vec![0]);
        assert_eq!(tree.jars_for_class("shared/Two"), vec![1]);
        assert_eq!(tree.jars_for_class("shared/Duplicate"), vec![0, 1]);
        assert!(tree.jars_for_class("shared/Missing").is_empty());
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
        // An absent class in a package no jar declares also misses.
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
        let coll = vec![type_name("kotlin/collections")];
        assert!(cp
            .functions_in_scope("emptyList", &coll)
            .iter()
            .any(|c| c.name == "emptyList"));
        // Out of scope: the same name does NOT resolve (kotlinc import visibility) — the lookup only
        // consults the given packages' facades, never the whole classpath.
        let text = vec![type_name("kotlin/text")];
        assert!(cp.functions_in_scope("emptyList", &text).is_empty());
    }

    #[test]
    fn extensions_in_scope_matches_scope_filtered_eager_index() {
        let Some(jar) = test_stdlib_jar() else {
            return;
        };
        let cp = Classpath::new(vec![jar]);
        let coll = vec![type_name("kotlin/collections")];
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
            .extensions_in_scope(recv, "map", &[type_name("kotlin/text")])
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
        let cp = Classpath::empty();
        assert!(cp.builtin_members.borrow().is_empty());
        assert!(cp.builtin_members("kotlin/String").is_empty());
        let string = type_name("kotlin/String");
        assert!(cp.builtin_members.borrow().contains_key(&string));
        assert!(cp.builtin_members("kotlin/String").is_empty());
    }

    #[test]
    fn stub_overlay_resolves_injected_class_by_name() {
        let stubs = crate::jvm::java_stub::stub_classes(
            &[("W.java".into(), "package p; public class Widget {}".into())],
            crate::jvm::java_stub::StubMode::Lenient,
            &|c| c == "java/lang/Object",
        )
        .expect("stub");
        let cp = Classpath::new(vec![]);
        assert!(cp.find("p/Widget").is_none(), "absent before overlay");
        cp.set_stub_overlay(stubs);
        let ci = cp.find("p/Widget").expect("resolved from overlay");
        assert_eq!(ci.this_class.render(), "p/Widget");
        cp.clear_stub_overlay();
        assert!(cp.find("p/Widget").is_none(), "gone after clear");
    }
}
