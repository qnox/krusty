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
use crate::types::Ty;

/// Map a Kotlin internal type name (`kotlin/Int`, `kotlin/Char`, …) from builtins metadata to a `Ty`.
fn kotlin_name_to_ty(name: &str) -> Ty {
    match name {
        "kotlin/Int" => Ty::Int,
        "kotlin/Char" => Ty::Char,
        "kotlin/Boolean" => Ty::Boolean,
        "kotlin/Long" => Ty::Long,
        "kotlin/Double" => Ty::Double,
        "kotlin/Float" => Ty::Float,
        "kotlin/Byte" => Ty::Byte,
        "kotlin/Short" => Ty::Short,
        "kotlin/String" => Ty::String,
        "kotlin/Unit" => Ty::Unit,
        _ => Ty::obj(name),
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

/// Process-global jimage index (name → file offset/size), keyed by the jimage path. The jimage is
/// identical for every compiled file, so parsing its 146 MB happens once per process, not per thread.
fn global_jimage_cache(
) -> &'static std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<HashMap<String, (u64, usize)>>>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<HashMap<String, (u64, usize)>>>>,
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

/// Process-global cache of parsed `ClassInfo` (internal name → parsed class, `None` if absent), keyed
/// by the classpath. The conformance harness compiles on several rayon worker threads, EACH with its
/// own `Classpath`; without sharing, every common class (`kotlin/collections/List`, …) was parsed once
/// per thread. Sharing this — like the type/ext/jimage indexes — parses each class once per process.
/// `RwLock` because reads (cache hits) dominate; a parse on a miss takes the write lock briefly.
type ClassCache = std::sync::Arc<std::sync::RwLock<HashMap<String, Option<ClassInfo>>>>;
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
    /// the bytecode inliner can splice it, but `resolve_callable` returns it only for inlining, never as
    /// a callable (an `invokestatic` to a package-private method would `IllegalAccessError`).
    pub public: bool,
}

/// Lazy index of static methods grouped by `(first_param_descriptor, method_name)`. Built on
/// first use from all entries in the classpath.
#[derive(Default)]
struct ExtIndex {
    /// `by_recv[recv_desc][method_name]` = list of candidates.
    by_recv: HashMap<String, HashMap<String, Vec<ExtCandidate>>>,
    /// `by_name[method_name]` = every public static method of that name (top-level functions and
    /// extensions alike), regardless of arity — for resolving receiver-less top-level calls (`listOf`).
    by_name: HashMap<String, Vec<ExtCandidate>>,
}

/// Full type index from the classpath: class names and Kotlin type aliases.
#[derive(Default, Clone, Debug)]
pub struct TypeIndex {
    /// Simple name → JVM internal name (e.g. `"StringBuilder"` → `"java/lang/StringBuilder"`).
    /// Only includes unambiguous mappings (if two classes share a simple name, neither appears).
    pub class_names: HashMap<String, String>,
    /// Kotlin type alias simple name → JVM internal name
    /// (e.g. `"StringBuilder"` → `"java/lang/StringBuilder"`).
    pub type_aliases: HashMap<String, String>,
}

/// Per-class `@Metadata` cache shape: class internal name → (function name → its single decoded Kotlin
/// type, e.g. the return type). Shared by [`Classpath::metadata_return_type`].
type MetaTypeCache = RefCell<HashMap<String, std::rc::Rc<HashMap<String, String>>>>;
/// Per-class `@Metadata` cache for an overloaded property: class internal name → (function name → ALL its
/// Kotlin extension-receiver names). Used by [`Classpath::metadata_receiver_types`].
type MetaReceiverCache = RefCell<HashMap<String, std::rc::Rc<HashMap<String, Vec<String>>>>>;
/// Per-class `@Metadata` cache for return-type nullability: class internal name → (function name →
/// whether its Kotlin return type is nullable `T?`). Used by [`Classpath::metadata_return_nullable`].
type MetaNullableCache = RefCell<HashMap<String, std::rc::Rc<HashMap<String, bool>>>>;
/// Per-class `@Metadata` cache: class internal name → (Kotlin function name → its `@JvmName`-mangled
/// overloads `[(jvm_name, jvm_desc, kotlin_return_class)]`). Bridges a Kotlin name to the JVM method that
/// `@OverloadResolutionByLambdaReturnType` selects (`sumOf` → `sumOfInt`/`sumOfLong`/…).
/// Kotlin function name → its `@JvmName`-mangled overloads, for one class's `@Metadata`.
type LambdaReturnOverloads = HashMap<String, Vec<super::metadata::JvmOverload>>;
type MetaOverloadCache = RefCell<HashMap<String, std::rc::Rc<LambdaReturnOverloads>>>;

#[derive(Default)]
pub struct Classpath {
    entries: Vec<Entry>,
    // Two-level parsed-class cache: `local` is a per-thread L1 (cheap `RefCell`, no lock — serves the
    // hot repeated lookups), backed by `shared` — a process-global L2 (`RwLock`) so a class is PARSED
    // once across all rayon worker threads, not once per thread. L1 miss → L2 → parse.
    local_cache: RefCell<HashMap<String, Option<ClassInfo>>>,
    cache: ClassCache,
    /// Open `ZipArchive` per jar path, so reading an entry is a central-directory hash lookup + inflate
    /// — NOT a re-parse of the whole central directory (which `zip::ZipArchive::new` does, thousands of
    /// entries for kotlin-stdlib). This is the classloader/javac strategy: parse each jar's directory
    /// once, then read class bytes lazily on demand. Profiling showed the per-read re-parse dominated
    /// type checking. Lives behind a `RefCell` (one `Classpath` per thread; never shared across threads).
    archives: RefCell<HashMap<PathBuf, zip::ZipArchive<File>>>,
    ext: RefCell<Option<std::sync::Arc<ExtIndex>>>,
    types: RefCell<Option<std::sync::Arc<TypeIndex>>>,
    /// Lazily-built index of the JDK jimage: internal class name → `(file offset, uncompressed size)`,
    /// so JDK class bytes can be seek-read on demand (the jimage stores classes uncompressed). Shared
    /// via `Arc` from a process-global cache so the 146 MB parse happens once.
    jimage: RefCell<Option<(PathBuf, std::sync::Arc<HashMap<String, (u64, usize)>>)>>,
    /// Cache of lazily-read method bodies (`(internal, name, descriptor) → MethodCode`), so the inline
    /// expander reads each inline function's body once even when it's called many times.
    bodies: RefCell<HashMap<(String, String, String), Option<MethodCode>>>,
    /// Cache of the `inline` function names declared by a class (from its `@Metadata`), so inline
    /// recognition at a call site doesn't re-decode the metadata per call.
    inline_names: RefCell<HashMap<String, std::rc::Rc<std::collections::HashSet<String>>>>,
    /// Cache of the `suspend` function names declared by a class (from its `@Metadata` `IS_SUSPEND`
    /// flag), so suspension-point recognition at a call site doesn't re-decode the metadata per call.
    suspend_names: RefCell<HashMap<String, std::rc::Rc<std::collections::HashSet<String>>>>,
    /// Cache of each class's `@Metadata` function name → Kotlin return-type internal name (decodes the
    /// read-only/mutable distinction the JVM signature erases — `mutableListOf` → `MutableList`).
    meta_returns: MetaTypeCache,
    /// Cache of each class's `@Metadata` function name → all Kotlin extension-RECEIVER internal names (the
    /// read-only/mutable identity the JVM signature erases — `plusAssign` → `[MutableCollection, MutableMap]`).
    meta_receivers: MetaReceiverCache,
    /// Cache of each class's `@Metadata` function name → whether its Kotlin return type is nullable
    /// (`takeIf`/`takeUnless` → `T?`). The JVM signature erases this; only `@Metadata` keeps it.
    meta_ret_nullable: MetaNullableCache,
    /// Cache of each class's `@Metadata` Kotlin-name → `@JvmName` overloads (see [`MetaOverloadCache`]).
    meta_overloads: MetaOverloadCache,
    /// Parsed `.kotlin_builtins` fragments, keyed by resource path (e.g. `kotlin/kotlin.kotlin_builtins`,
    /// `kotlin/collections/collections.kotlin_builtins`), each mapping class internal name → its
    /// supertypes + members. Built once per file on first use — the single source for BOTH the collection
    /// read-only/mutable hierarchy AND every builtin type's API. Empty if no stdlib is on the classpath.
    builtins: RefCell<HashMap<String, std::rc::Rc<HashMap<String, super::metadata::BuiltinClass>>>>,
}

impl Classpath {
    pub fn new(paths: Vec<PathBuf>) -> Classpath {
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
        Classpath {
            entries,
            local_cache: RefCell::new(HashMap::new()),
            cache: global_class_cache(&cache_key),
            archives: RefCell::new(HashMap::new()),
            ext: RefCell::new(None),
            types: RefCell::new(None),
            jimage: RefCell::new(None),
            bodies: RefCell::new(HashMap::new()),
            inline_names: RefCell::new(HashMap::new()),
            suspend_names: RefCell::new(HashMap::new()),
            meta_returns: RefCell::new(HashMap::new()),
            meta_receivers: RefCell::new(HashMap::new()),
            meta_ret_nullable: RefCell::new(HashMap::new()),
            meta_overloads: RefCell::new(HashMap::new()),
            builtins: RefCell::new(HashMap::new()),
        }
    }

    /// The Kotlin return-type internal name of function `fn_name` declared in class `internal`, decoded
    /// from `@Metadata` (`mutableListOf` → `kotlin/collections/MutableList` vs `listOf` → `…/List`). The
    /// JVM descriptor/`Signature` erase both to `java/util/List`; only `@Metadata` carries the distinction.
    /// A multifile FACADE (`CollectionsKt`) has no function metadata of its own — its `@Metadata` `d1`
    /// lists the PART class names, which hold the functions; merge the parts.
    pub fn metadata_return_type(&self, internal: &str, fn_name: &str) -> Option<String> {
        self.metadata_fn_type(
            &self.meta_returns,
            internal,
            fn_name,
            super::metadata::package_function_return_types,
        )
    }

    /// The LOGICAL Kotlin return type of `internal.fn_name` as a `Ty`, from `@Metadata`. Used for a
    /// `suspend fun`, whose physical JVM method erases the return to `Object` (the resume value) — only
    /// `@Metadata` carries the real return (`suspend fun helper(): Int` → `Ty::Int`). A nullable
    /// primitive return (`Int?`) stays boxed, so it maps to its wrapper object type. `None` if the
    /// metadata has no class return type recorded.
    pub fn metadata_return_ty(&self, internal: &str, fn_name: &str) -> Option<Ty> {
        let name = self.metadata_return_type(internal, fn_name)?;
        let ty = kotlin_name_to_ty(&name);
        if ty.is_primitive() && self.metadata_return_nullable(internal, fn_name) {
            return super::jvm_class_map::wrapper_internal(ty).map(Ty::obj);
        }
        Some(ty)
    }

    /// All Kotlin extension-receiver internal names of `fn_name` in `internal` (`plusAssign` →
    /// `[kotlin/collections/MutableCollection, …/MutableMap]`), from `@Metadata`. A name is overloaded
    /// across receivers, so a receiver applies if it is a subtype of ANY entry. The JVM signature erases
    /// the receiver to its first parameter; only `@Metadata` keeps the read-only/mutable identity. Empty
    /// for a non-extension function.
    pub fn metadata_receiver_types(&self, internal: &str, fn_name: &str) -> Vec<String> {
        if let Some(m) = self.meta_receivers.borrow().get(internal) {
            return m.get(fn_name).cloned().unwrap_or_default();
        }
        let ci = self.find(internal);
        let mut map = ci
            .as_ref()
            .map(super::metadata::package_function_receivers)
            .unwrap_or_default();
        if map.is_empty() {
            if let Some(ci) = &ci {
                for part in &ci.kotlin_d1 {
                    if let Some(pci) = self.find(part) {
                        // UNION across parts: a name (`forEach`) is overloaded across receivers
                        // (`Iterable` in one part, `Iterator`/`CharSequence`/… in others), so the
                        // receiver lists must concatenate — first-wins would drop the supertype that
                        // actually applies and wrongly reject the call.
                        for (k, v) in super::metadata::package_function_receivers(&pci) {
                            let e = map.entry(k).or_default();
                            for x in v {
                                if !e.contains(&x) {
                                    e.push(x);
                                }
                            }
                        }
                    }
                }
            }
        }
        let rc = std::rc::Rc::new(map);
        let hit = rc.get(fn_name).cloned().unwrap_or_default();
        self.meta_receivers
            .borrow_mut()
            .insert(internal.to_string(), rc);
        hit
    }

    /// Whether function `fn_name` in class `internal` has a NULLABLE Kotlin return type (`takeIf`/
    /// `takeUnless` → `T?`), from `@Metadata`. The JVM descriptor/`Signature` erase nullability; only
    /// `@Metadata` carries it. A multifile FACADE has no function metadata of its own — merge its parts.
    pub fn metadata_return_nullable(&self, internal: &str, fn_name: &str) -> bool {
        if let Some(m) = self.meta_ret_nullable.borrow().get(internal) {
            return m.get(fn_name).copied().unwrap_or(false);
        }
        let ci = self.find(internal);
        let mut map = ci
            .as_ref()
            .map(super::metadata::package_function_return_nullable)
            .unwrap_or_default();
        if map.is_empty() {
            if let Some(ci) = &ci {
                for part in &ci.kotlin_d1 {
                    if let Some(pci) = self.find(part) {
                        for (k, v) in super::metadata::package_function_return_nullable(&pci) {
                            let e = map.entry(k).or_insert(false);
                            *e = *e || v;
                        }
                    }
                }
            }
        }
        let rc = std::rc::Rc::new(map);
        let hit = rc.get(fn_name).copied().unwrap_or(false);
        self.meta_ret_nullable
            .borrow_mut()
            .insert(internal.to_string(), rc);
        hit
    }

    /// A facade class's `@Metadata` Kotlin-name → `@JvmName` overloads, cached (part-merged for a multifile
    /// facade). See [`MetaOverloadCache`].
    pub fn lambda_return_overloads(&self, internal: &str) -> std::rc::Rc<LambdaReturnOverloads> {
        if let Some(m) = self.meta_overloads.borrow().get(internal) {
            return m.clone();
        }
        // Overloads of one Kotlin name are split across the multifile facade's PART classes (the
        // `Int`/`Long`/`Double` `sumOf` in one part, `UInt`/`ULong` in another). The facade EXTENDS its
        // parts, so union every class's own metadata up the superclass chain — exactly how the extension
        // index reaches the part methods (a part isn't listed in the facade's `d1`).
        let mut map: LambdaReturnOverloads = HashMap::new();
        let mut cur = Some(internal.to_string());
        let mut seen = std::collections::HashSet::new();
        while let Some(cn) = cur {
            if !seen.insert(cn.clone()) {
                break;
            }
            let Some(ci) = self.find(&cn) else { break };
            for (k, v) in super::metadata::package_lambda_return_overloads(&ci) {
                map.entry(k).or_default().extend(v);
            }
            cur = ci.super_class.clone();
        }
        let rc = std::rc::Rc::new(map);
        self.meta_overloads
            .borrow_mut()
            .insert(internal.to_string(), rc.clone());
        rc
    }

    /// Every distinct owner (facade) that declares a static method whose first parameter matches
    /// `receiver_desc` — the facades to consult for a Kotlin-name resolution (`sumOf`).
    pub fn find_extension_owners(&self, receiver_desc: &str) -> Vec<String> {
        self.ensure_ext_index();
        let mut owners: Vec<String> = Vec::new();
        if let Some(idx) = self.ext.borrow().as_ref() {
            if let Some(by_name) = idx.by_recv.get(receiver_desc) {
                for cands in by_name.values() {
                    for c in cands {
                        if !owners.contains(&c.owner) {
                            owners.push(c.owner.clone());
                        }
                    }
                }
            }
        }
        owners
    }

    /// Shared cache+decode for the per-function `@Metadata` type lookups: decode the class's `Package`
    /// metadata with `decode` (return type / receiver type), and — for a multifile FACADE that has no
    /// function metadata of its own — merge the part classes named in its `d1`.
    fn metadata_fn_type(
        &self,
        cache: &MetaTypeCache,
        internal: &str,
        fn_name: &str,
        decode: impl Fn(&ClassInfo) -> HashMap<String, String>,
    ) -> Option<String> {
        if let Some(m) = cache.borrow().get(internal) {
            return m.get(fn_name).cloned();
        }
        let ci = self.find(internal);
        let mut map = ci.as_ref().map(&decode).unwrap_or_default();
        if map.is_empty() {
            if let Some(ci) = &ci {
                for part in &ci.kotlin_d1 {
                    if let Some(pci) = self.find(part) {
                        for (k, v) in decode(&pci) {
                            map.entry(k).or_insert(v);
                        }
                    }
                }
            }
        }
        let rc = std::rc::Rc::new(map);
        let hit = rc.get(fn_name).cloned();
        cache.borrow_mut().insert(internal.to_string(), rc);
        hit
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

    /// Resolve a builtin type's member return `Ty` by name + argument types, straight from its builtins
    /// declarations (no per-type hardcoded table). `None` if no member of that name/arity is declared
    /// there (e.g. a `StringsKt` EXTENSION on `String` lives in package metadata, resolved elsewhere).
    pub fn builtin_member_ret(&self, internal: &str, name: &str, args: &[Ty]) -> Option<Ty> {
        let path = Self::builtins_path_for(internal);
        let f = self.builtins_file(&path);
        let m = f
            .get(internal)?
            .members
            .iter()
            .find(|m| m.name == name && m.params.len() == args.len())?;
        Some(kotlin_name_to_ty(&m.ret))
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
        // Track ambiguous simple names so we can remove them.
        let mut ambiguous: std::collections::HashSet<String> = std::collections::HashSet::new();

        for e in &self.entries {
            match e {
                Entry::Dir(d) => scan_types_dir(d, &mut idx, &mut ambiguous),
                Entry::Jar(j) => scan_types_jar(j, &mut idx, &mut ambiguous),
                Entry::Jimage(p) => scan_types_jimage(p, &mut idx, &mut ambiguous),
            }
        }
        // Remove ambiguous simple names that map to multiple internal names.
        for name in &ambiguous {
            idx.class_names.remove(name.as_str());
        }
        let idx = std::sync::Arc::new(idx);
        global_type_cache().lock().unwrap().insert(key, idx.clone());
        *self.types.borrow_mut() = Some(idx.clone());
        idx
    }

    /// Seek-read a class's bytes from the JDK jimage (uncompressed entry), via the lazily-built index.
    fn jimage_bytes(&self, internal: &str) -> Option<Vec<u8>> {
        self.ensure_jimage_index();
        let guard = self.jimage.borrow();
        let (path, index) = guard.as_ref()?;
        let &(offset, size) = index.get(internal)?;
        use std::io::{Read, Seek, SeekFrom};
        let mut f = File::open(path).ok()?;
        f.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = vec![0u8; size];
        f.read_exact(&mut buf).ok()?;
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

    pub fn find(&self, internal: &str) -> Option<ClassInfo> {
        // The front end names built-in types in Kotlin terms (`kotlin/Any`); a classpath artifact is
        // a real JVM class, so map to the JVM name (`java/lang/Object`) before looking it up.
        let internal = super::jvm_class_map::to_jvm_internal(internal);
        // L1: per-thread, no lock.
        if let Some(hit) = self.local_cache.borrow().get(internal) {
            return hit.clone();
        }
        // L2: process-global, shared across threads — a class parsed by ANY thread is reused here.
        if let Some(hit) = self.cache.read().unwrap().get(internal).cloned() {
            self.local_cache
                .borrow_mut()
                .insert(internal.to_string(), hit.clone());
            return hit;
        }
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
                    found = Some(ci);
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
        if let Some(hit) = self.bodies.borrow().get(&key) {
            return hit.clone();
        }
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

    /// Whether `internal.name(...)` is a Kotlin `inline` function, per the class's `@Metadata` (the
    /// inline-name set is decoded once per class and cached). The call site uses this to decide
    /// whether to expand the body rather than emit a call.
    pub fn is_inline_method(&self, internal: &str, name: &str) -> bool {
        if let Some(set) = self.inline_names.borrow().get(internal) {
            return set.contains(name);
        }
        let ci = self.find(internal);
        let mut names = ci
            .as_ref()
            .map(super::metadata::inline_method_names)
            .unwrap_or_default();
        // A multifile facade has no function metadata — `inline` flags live in its part classes, which it
        // *extends* (a superclass chain). Merge their inline names in.
        let mut cur = ci.as_ref().and_then(|ci| ci.super_class.clone());
        while let Some(s) = cur {
            if s == "java/lang/Object" {
                break;
            }
            match self.find(&s) {
                Some(pci) => {
                    names.extend(super::metadata::inline_method_names(&pci));
                    cur = pci.super_class.clone();
                }
                None => break,
            }
        }
        let set = std::rc::Rc::new(names);
        let hit = set.contains(name);
        self.inline_names
            .borrow_mut()
            .insert(internal.to_string(), set);
        hit
    }

    /// Whether `internal.name(...)` is a Kotlin `suspend` function, per the class's `@Metadata`
    /// `IS_SUSPEND` flag (decoded once per class and cached). A call to it is a coroutine suspension
    /// point. Mirrors [`is_inline_method`](Self::is_inline_method), including the multifile-facade
    /// part-class superclass walk.
    pub fn is_suspend_method(&self, internal: &str, name: &str) -> bool {
        if let Some(set) = self.suspend_names.borrow().get(internal) {
            return set.contains(name);
        }
        let ci = self.find(internal);
        let mut names = ci
            .as_ref()
            .map(super::metadata::suspend_method_names)
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
        self.ext
            .borrow()
            .as_ref()
            .and_then(|idx| idx.by_recv.get(receiver_desc))
            .and_then(|by_name| by_name.get(method_name))
            .cloned()
            .unwrap_or_default()
    }

    /// Every static method named `method_name` across the classpath (top-level functions and
    /// extensions), for resolving a receiver-less call. Includes non-public (`@InlineOnly`) candidates,
    /// each tagged via `ExtCandidate.public`; the caller filters — normal resolution is public-only.
    pub fn find_top_level(&self, method_name: &str) -> Vec<ExtCandidate> {
        self.ensure_ext_index();
        self.ext
            .borrow()
            .as_ref()
            .and_then(|idx| idx.by_name.get(method_name))
            .cloned()
            .unwrap_or_default()
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
        // Pass 2: index the public static methods reachable from each PUBLIC class — its own and those
        // inherited through its superclass chain (the multifile parts) — with owner = the public class
        // (the facade), which is what an `invokestatic` resolves through, like kotlinc.
        let mut idx = ExtIndex::default();
        for (name, lite) in &all {
            if !lite.is_public {
                continue;
            }
            let mut cur = Some(name.clone());
            let mut visited = std::collections::HashSet::new();
            while let Some(cn) = cur {
                if !visited.insert(cn.clone()) {
                    break;
                }
                let Some(c) = all.get(&cn) else { break };
                for (mname, mdesc, msig, public) in &c.statics {
                    let Some(ret_desc) = descriptor_ret(mdesc) else {
                        continue;
                    };
                    let cand = ExtCandidate {
                        owner: name.clone(),
                        name: mname.clone(),
                        descriptor: mdesc.clone(),
                        ret_desc,
                        signature: msig.clone(),
                        public: *public,
                    };
                    // A receiver-less top-level function (no first param) is by_name-only.
                    if let Some(first_param) = first_descriptor_param(mdesc) {
                        idx.by_recv
                            .entry(first_param)
                            .or_default()
                            .entry(mname.clone())
                            .or_default()
                            .push(cand.clone());
                    }
                    idx.by_name.entry(mname.clone()).or_default().push(cand);
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
}

/// A lean per-class record for building the extension index — only what's needed to follow facade
/// superclass chains and index static methods (no fields, no instance methods).
struct ClassLite {
    is_public: bool,
    super_class: Option<String>,
    /// `(name, descriptor, generic-signature, is_public)` of each static method (excl `<init>`/`<clinit>`).
    /// Non-public ones (`@InlineOnly`) are kept for the inliner; the flag gates normal resolution.
    statics: Vec<(String, String, Option<String>, bool)>,
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
    all.insert(
        ci.this_class.clone(),
        ClassLite {
            is_public: ci.is_public(),
            super_class: ci.super_class,
            statics,
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

/// Extract the first parameter type from a JVM method descriptor like `(Ljava/lang/String;IZ)V`.
/// Returns `None` if there are no parameters.
fn first_descriptor_param(desc: &str) -> Option<String> {
    let inner = desc.strip_prefix('(')?;
    let mut s = inner;
    if s.starts_with(')') {
        return None; // no params
    }
    Some(read_one_type(&mut s).to_string())
}

/// Extract the return type descriptor from a JVM method descriptor.
fn descriptor_ret(desc: &str) -> Option<String> {
    let close = desc.find(')')?;
    Some(desc[close + 1..].to_string())
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

/// Register `internal` (e.g. `java/lang/StringBuilder`) into the simple-name → internal index,
/// tracking ambiguity. **Name-based** — does not parse the class file. This is the lazy path:
/// kotlinc/javac likewise index by entry/package name and only read a `.class` when its members
/// are actually needed (see `find`).
fn register_class_name(
    internal: &str,
    idx: &mut TypeIndex,
    ambiguous: &mut std::collections::HashSet<String>,
) {
    if internal.is_empty() {
        return;
    }
    let simple = internal.rsplit('/').next().unwrap_or(internal);
    // Skip synthetic/anonymous/nested (`$`) and module/package descriptors.
    if simple.contains('$') || simple == "module-info" || simple == "package-info" {
        return;
    }
    // A fully-qualified type reference (`kotlin.time.TimeSource`) resolves WITHOUT an import — Kotlin
    // always permits the fully-qualified name. Register the dotted FQ form → internal; it is unique
    // (one class per FQ name), so it never participates in the simple-name ambiguity pruning.
    if internal.contains('/') {
        idx.class_names
            .entry(internal.replace('/', "."))
            .or_insert_with(|| internal.to_string());
    }
    match idx.class_names.get(simple) {
        Some(existing) if existing != internal => {
            // A `kotlin/*` type WINS its simple name over a non-kotlin one — mirrors kotlinc, where the
            // `kotlin.*` packages are default-imported, so `Continuation` means `kotlin/coroutines/
            // Continuation`, not the JVM's `jdk/internal/vm/Continuation`. Only a clash between two
            // same-tier (both `kotlin/*`, or both non-kotlin) types is genuinely ambiguous → pruned.
            let existing_kotlin = existing.starts_with("kotlin/");
            let new_kotlin = internal.starts_with("kotlin/");
            if new_kotlin && !existing_kotlin {
                idx.class_names
                    .insert(simple.to_string(), internal.to_string());
                ambiguous.remove(simple);
            } else if existing_kotlin && !new_kotlin {
                // keep the kotlin winner already recorded
            } else {
                ambiguous.insert(simple.to_string());
            }
        }
        Some(_) => {}
        None => {
            idx.class_names
                .insert(simple.to_string(), internal.to_string());
        }
    }
}

/// `Xxx.class` entry name (jar/jimage path) → internal name, or `None` if not an indexable class.
fn class_internal_from_entry(name: &str) -> Option<&str> {
    name.strip_suffix(".class").filter(|s| !s.is_empty())
}

/// Parse Kotlin type aliases from a `*TypeAliasesKt.class` file's `@Metadata` `d2` array. Only such
/// files are ever parsed for the type index — every other class is indexed by name alone.
fn parse_aliases_from_bytes(bytes: &[u8], idx: &mut TypeIndex) {
    let Ok(ci) = parse_class(bytes) else { return };
    if ci.kotlin_d2.is_empty() {
        return;
    }
    let alias_names: Vec<String> = ci
        .methods
        .iter()
        .filter(|m| m.name.ends_with("$annotations"))
        .map(|m| m.name.trim_end_matches("$annotations").to_string())
        .collect();
    // In d2, alias name and its JVM descriptor appear as consecutive strings:
    // name → "Lsome/Target;" (a JVM class descriptor).
    let d2 = &ci.kotlin_d2;
    for alias in &alias_names {
        for i in 0..d2.len() {
            if d2[i] == *alias {
                if let Some(desc) = d2.get(i + 1) {
                    if let Some(internal_name) = desc_to_internal(desc) {
                        idx.type_aliases.insert(alias.clone(), internal_name);
                    }
                }
                break;
            }
        }
    }
}

fn is_type_aliases_kt(internal: &str) -> bool {
    internal
        .rsplit('/')
        .next()
        .unwrap_or(internal)
        .ends_with("TypeAliasesKt")
}

/// Convert a JVM class descriptor `Lsome/Class;` to internal name `some/Class`.
fn desc_to_internal(desc: &str) -> Option<String> {
    let s = desc.strip_prefix('L')?.strip_suffix(';')?;
    if s.is_empty() {
        return None;
    }
    Some(s.to_string())
}

fn scan_types_dir(
    dir: &Path,
    idx: &mut TypeIndex,
    ambiguous: &mut std::collections::HashSet<String>,
) {
    scan_types_dir_rooted(dir, dir, idx, ambiguous);
}

/// Walk `dir`, registering each `*.class` by its path relative to `root` (the internal name).
/// Only `*TypeAliasesKt.class` files are read+parsed (for aliases); all others are name-only.
fn scan_types_dir_rooted(
    root: &Path,
    dir: &Path,
    idx: &mut TypeIndex,
    ambiguous: &mut std::collections::HashSet<String>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            scan_types_dir_rooted(root, &p, idx, ambiguous);
        } else if p.extension().map_or(false, |x| x == "class") {
            let Ok(rel) = p.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            let Some(internal) = class_internal_from_entry(&rel) else {
                continue;
            };
            register_class_name(internal, idx, ambiguous);
            if is_type_aliases_kt(internal) {
                if let Ok(b) = std::fs::read(&p) {
                    parse_aliases_from_bytes(&b, idx);
                }
            }
        }
    }
}

fn scan_types_jar(
    jar: &Path,
    idx: &mut TypeIndex,
    ambiguous: &mut std::collections::HashSet<String>,
) {
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
        register_class_name(internal, idx, ambiguous);
        // Parse bytes only for the rare alias-carrier classes — everything else is name-only.
        if is_type_aliases_kt(internal) {
            let mut buf = Vec::new();
            if entry.read_to_end(&mut buf).is_ok() {
                parse_aliases_from_bytes(&buf, idx);
            }
        }
    }
}

/// Index class names from a JDK `lib/modules` jimage. Name-only (no class parsing), reading the
/// jimage location table directly — the bootclasspath equivalent of a jar's central directory.
/// Format reference (little-endian header): jdk.internal.jimage.BasicImageReader / ImageHeader /
/// ImageLocation. Inner classes (`A$B`) and ambiguous simple names are dropped, like any entry.
/// Build the jimage class index: internal name → `(absolute file offset, uncompressed size)` for each
/// uncompressed `.class` resource. Mirrors `scan_types_jimage`'s navigation but keeps content offsets.
fn build_jimage_index(path: &Path) -> Option<HashMap<String, (u64, usize)>> {
    let b = std::fs::read(path).ok()?;
    if b.len() < 28 {
        return None;
    }
    let u32le = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    if u32le(0) != 0xCAFE_DADA {
        return None;
    }
    let table_length = u32le(16) as usize;
    let locations_size = u32le(20) as usize;
    let strings_size = u32le(24) as usize;
    let header = 28;
    let offsets = header + table_length * 4;
    let locations = offsets + table_length * 4;
    let strings = locations + locations_size;
    let content = strings + strings_size;
    if content > b.len() {
        return None;
    }
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
        if comp != 0 {
            continue; // compressed jimage entry — not handled (this JDK stores classes uncompressed)
        }
        idx.entry(internal).or_insert(((content + off) as u64, unc));
    }
    Some(idx)
}

fn scan_types_jimage(
    path: &Path,
    idx: &mut TypeIndex,
    ambiguous: &mut std::collections::HashSet<String>,
) {
    let Ok(b) = std::fs::read(path) else { return };
    if b.len() < 28 {
        return;
    }
    let u32le = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    if u32le(0) != 0xCAFE_DADA {
        return;
    }
    let table_length = u32le(16) as usize;
    let locations_size = u32le(20) as usize;
    let header = 28;
    let offsets = header + table_length * 4; // skip redirect table (table_length × i32)
    let locations = offsets + table_length * 4;
    let strings = locations + locations_size;
    if strings > b.len() {
        return;
    }
    // A jimage string is NUL-terminated modified-UTF8 at `strings + off` (off 0 = empty).
    let read_str = |off: usize| -> &str {
        if off == 0 {
            return "";
        }
        let start = strings + off;
        let mut e = start;
        while e < b.len() && b[e] != 0 {
            e += 1;
        }
        std::str::from_utf8(&b[start..e]).unwrap_or("")
    };
    // Decode an ImageLocation attribute stream into (module, parent, base, extension) string offsets.
    let decode = |mut p: usize| -> (usize, usize, usize, usize) {
        let (mut m, mut par, mut base, mut ext) = (0usize, 0usize, 0usize, 0usize);
        while p < b.len() {
            let byte = b[p];
            p += 1;
            let kind = byte >> 3;
            if kind == 0 {
                break;
            } // ATTRIBUTE_END
            let len = ((byte & 0x7) + 1) as usize;
            let mut v = 0usize;
            for _ in 0..len {
                if p >= b.len() {
                    break;
                }
                v = (v << 8) | b[p] as usize;
                p += 1;
            }
            match kind {
                1 => m = v,    // MODULE
                2 => par = v,  // PARENT (package, '/'-separated)
                3 => base = v, // BASE (simple file name, incl. extension separator handling below)
                4 => ext = v,  // EXTENSION
                _ => {} // OFFSET/COMPRESSED/UNCOMPRESSED — content attrs, unused for the index
            }
        }
        (m, par, base, ext)
    };
    for i in 0..table_length {
        let loc_off = u32le(offsets + i * 4) as usize;
        if loc_off == 0 {
            continue;
        }
        let (m, par, base, ext) = decode(locations + loc_off);
        // Index java module classes (`java.base`, `java.*`); skip the JDK's own `jdk.*`/`sun.*`
        // implementation modules' resources only by what they expose by name + ambiguity rules.
        if read_str(ext) != "class" {
            continue;
        }
        let parent = read_str(par);
        if parent.is_empty() {
            continue;
        }
        let internal = format!("{parent}/{}", read_str(base));
        let _ = m;
        register_class_name(&internal, idx, ambiguous);
    }
}

#[cfg(test)]
mod fq_tests {
    use super::*;

    #[test]
    fn registers_fully_qualified_name() {
        let mut idx = TypeIndex::default();
        let mut amb = std::collections::HashSet::new();
        register_class_name("kotlin/time/TimeSource", &mut idx, &mut amb);
        // Both the simple name AND the dotted fully-qualified name resolve to the internal — a FQ type
        // reference needs no import.
        assert_eq!(
            idx.class_names.get("TimeSource").map(String::as_str),
            Some("kotlin/time/TimeSource")
        );
        assert_eq!(
            idx.class_names
                .get("kotlin.time.TimeSource")
                .map(String::as_str),
            Some("kotlin/time/TimeSource")
        );
        // A second class with the same simple name: the simple name is contested (here `kotlin/*` wins,
        // mirroring kotlinc's default imports), but each distinct FQ name stays independently resolvable.
        register_class_name("com/example/TimeSource", &mut idx, &mut amb);
        assert_eq!(
            idx.class_names
                .get("com.example.TimeSource")
                .map(String::as_str),
            Some("com/example/TimeSource")
        );
        assert_eq!(
            idx.class_names
                .get("kotlin.time.TimeSource")
                .map(String::as_str),
            Some("kotlin/time/TimeSource")
        );

        // Two genuinely ambiguous (same-tier) simple names ARE pruned, yet both FQ names still resolve.
        register_class_name("a/b/Widget", &mut idx, &mut amb);
        register_class_name("c/d/Widget", &mut idx, &mut amb);
        assert!(amb.contains("Widget"), "same-tier simple name is ambiguous");
        assert_eq!(
            idx.class_names.get("a.b.Widget").map(String::as_str),
            Some("a/b/Widget")
        );
        assert_eq!(
            idx.class_names.get("c.d.Widget").map(String::as_str),
            Some("c/d/Widget")
        );
    }
}
