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

#[derive(Default)]
pub struct Classpath {
    entries: Vec<Entry>,
    cache: RefCell<HashMap<String, Option<ClassInfo>>>,
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
}

impl Classpath {
    pub fn new(paths: Vec<PathBuf>) -> Classpath {
        let entries = paths
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
        Classpath {
            entries,
            cache: RefCell::new(HashMap::new()),
            ext: RefCell::new(None),
            types: RefCell::new(None),
            jimage: RefCell::new(None),
            bodies: RefCell::new(HashMap::new()),
            inline_names: RefCell::new(HashMap::new()),
        }
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

    pub fn find(&self, internal: &str) -> Option<ClassInfo> {
        // The front end names built-in types in Kotlin terms (`kotlin/Any`); a classpath artifact is
        // a real JVM class, so map to the JVM name (`java/lang/Object`) before looking it up.
        let internal = super::jvm_class_map::to_jvm_internal(internal);
        if let Some(hit) = self.cache.borrow().get(internal) {
            return hit.clone();
        }
        let name = format!("{internal}.class");
        let mut found = None;
        for e in &self.entries {
            let bytes = match e {
                Entry::Dir(d) => std::fs::read(d.join(&name)).ok(),
                Entry::Jar(j) => read_jar_entry(j, &name),
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
                Entry::Jar(j) => read_jar_entry(j, &name),
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
        if !entry.name().ends_with(".class") {
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

fn read_jar_entry(jar: &Path, name: &str) -> Option<Vec<u8>> {
    let f = File::open(jar).ok()?;
    let mut archive = zip::ZipArchive::new(f).ok()?;
    let mut entry = archive.by_name(name).ok()?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
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
    match idx.class_names.get(simple) {
        Some(existing) if existing != internal => {
            ambiguous.insert(simple.to_string());
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
