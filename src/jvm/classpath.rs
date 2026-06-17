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

use crate::jvm::classreader::{parse_class, ClassInfo};

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
fn global_type_cache() -> &'static std::sync::Mutex<HashMap<Vec<PathBuf>, std::sync::Arc<TypeIndex>>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<Vec<PathBuf>, std::sync::Arc<TypeIndex>>>> = std::sync::OnceLock::new();
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
}

/// Lazy index of static methods grouped by `(first_param_descriptor, method_name)`. Built on
/// first use from all entries in the classpath.
#[derive(Default)]
struct ExtIndex {
    /// `by_recv[recv_desc][method_name]` = list of candidates.
    by_recv: HashMap<String, HashMap<String, Vec<ExtCandidate>>>,
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
    ext: RefCell<Option<ExtIndex>>,
    types: RefCell<Option<std::sync::Arc<TypeIndex>>>,
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
        Classpath { entries, cache: RefCell::new(HashMap::new()), ext: RefCell::new(None), types: RefCell::new(None) }
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
        let key: Vec<PathBuf> = self.entries.iter().map(|e| e.path().to_path_buf()).collect();
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

    pub fn find(&self, internal: &str) -> Option<ClassInfo> {
        if let Some(hit) = self.cache.borrow().get(internal) {
            return hit.clone();
        }
        let name = format!("{internal}.class");
        let mut found = None;
        for e in &self.entries {
            let bytes = match e {
                Entry::Dir(d) => std::fs::read(d.join(&name)).ok(),
                Entry::Jar(j) => read_jar_entry(j, &name),
                // Reading class bytes from the jimage (lazy member resolution for JDK types) is a
                // follow-up: it needs jimage content extraction + decompression. For now JDK types
                // resolve as types but their members stay unresolved (rejected, never miscompiled).
                Entry::Jimage(_) => None,
            };
            if let Some(b) = bytes {
                if let Ok(ci) = parse_class(&b) {
                    found = Some(ci);
                    break;
                }
            }
        }
        self.cache.borrow_mut().insert(internal.to_string(), found.clone());
        found
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

    fn ensure_ext_index(&self) {
        if self.ext.borrow().is_some() {
            return;
        }
        let mut idx = ExtIndex::default();
        for e in &self.entries {
            match e {
                Entry::Dir(d) => index_dir(d, &mut idx),
                Entry::Jar(j) => index_jar(j, &mut idx),
                // No extension functions live in the JDK (Kotlin extensions come from stdlib jars).
                Entry::Jimage(_) => {}
            }
        }
        *self.ext.borrow_mut() = Some(idx);
    }
}

fn index_class_bytes(bytes: &[u8], idx: &mut ExtIndex) {
    let Ok(ci) = parse_class(bytes) else { return };
    // Only public methods on a public class are callable from generated code — a non-public
    // member (e.g. a multifile-facade *part* class, or a private overload) would `IllegalAccessError`
    // at runtime. Skip them so such a call stays unresolved (rejected) rather than miscompiled.
    if !ci.is_public() {
        return;
    }
    for m in &ci.methods {
        if !m.is_static() || !m.is_public() || m.name.starts_with('<') {
            continue;
        }
        // Parse first parameter from descriptor `(Lfoo/Bar;II)V` → `Lfoo/Bar;`
        let Some(first_param) = first_descriptor_param(&m.descriptor) else { continue };
        // Return type.
        let Some(ret_desc) = descriptor_ret(&m.descriptor) else { continue };
        let cand = ExtCandidate {
            owner: ci.this_class.clone(),
            name: m.name.clone(),
            descriptor: m.descriptor.clone(),
            ret_desc,
        };
        idx.by_recv
            .entry(first_param)
            .or_default()
            .entry(m.name.clone())
            .or_default()
            .push(cand);
    }
}

fn index_dir(dir: &Path, idx: &mut ExtIndex) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            index_dir(&p, idx);
        } else if p.extension().map_or(false, |x| x == "class") {
            if let Ok(b) = std::fs::read(&p) {
                index_class_bytes(&b, idx);
            }
        }
    }
}

fn index_jar(jar: &Path, idx: &mut ExtIndex) {
    let Ok(f) = File::open(jar) else { return };
    let Ok(mut archive) = zip::ZipArchive::new(f) else { return };
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else { continue };
        if !entry.name().ends_with(".class") {
            continue;
        }
        let mut buf = Vec::new();
        if entry.read_to_end(&mut buf).is_ok() {
            index_class_bytes(&buf, idx);
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
fn register_class_name(internal: &str, idx: &mut TypeIndex, ambiguous: &mut std::collections::HashSet<String>) {
    if internal.is_empty() { return; }
    let simple = internal.rsplit('/').next().unwrap_or(internal);
    // Skip synthetic/anonymous/nested (`$`) and module/package descriptors.
    if simple.contains('$') || simple == "module-info" || simple == "package-info" { return; }
    match idx.class_names.get(simple) {
        Some(existing) if existing != internal => { ambiguous.insert(simple.to_string()); }
        Some(_) => {}
        None => { idx.class_names.insert(simple.to_string(), internal.to_string()); }
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
    if ci.kotlin_d2.is_empty() { return; }
    let alias_names: Vec<String> = ci.methods.iter()
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
    internal.rsplit('/').next().unwrap_or(internal).ends_with("TypeAliasesKt")
}

/// Convert a JVM class descriptor `Lsome/Class;` to internal name `some/Class`.
fn desc_to_internal(desc: &str) -> Option<String> {
    let s = desc.strip_prefix('L')?.strip_suffix(';')?;
    if s.is_empty() { return None; }
    Some(s.to_string())
}

fn scan_types_dir(dir: &Path, idx: &mut TypeIndex, ambiguous: &mut std::collections::HashSet<String>) {
    scan_types_dir_rooted(dir, dir, idx, ambiguous);
}

/// Walk `dir`, registering each `*.class` by its path relative to `root` (the internal name).
/// Only `*TypeAliasesKt.class` files are read+parsed (for aliases); all others are name-only.
fn scan_types_dir_rooted(root: &Path, dir: &Path, idx: &mut TypeIndex, ambiguous: &mut std::collections::HashSet<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            scan_types_dir_rooted(root, &p, idx, ambiguous);
        } else if p.extension().map_or(false, |x| x == "class") {
            let Ok(rel) = p.strip_prefix(root) else { continue };
            let rel = rel.to_string_lossy().replace('\\', "/");
            let Some(internal) = class_internal_from_entry(&rel) else { continue };
            register_class_name(internal, idx, ambiguous);
            if is_type_aliases_kt(internal) {
                if let Ok(b) = std::fs::read(&p) {
                    parse_aliases_from_bytes(&b, idx);
                }
            }
        }
    }
}

fn scan_types_jar(jar: &Path, idx: &mut TypeIndex, ambiguous: &mut std::collections::HashSet<String>) {
    let Ok(f) = File::open(jar) else { return };
    let Ok(mut archive) = zip::ZipArchive::new(f) else { return };
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else { continue };
        let name = entry.name().to_string();
        let Some(internal) = class_internal_from_entry(&name) else { continue };
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
fn scan_types_jimage(path: &Path, idx: &mut TypeIndex, ambiguous: &mut std::collections::HashSet<String>) {
    let Ok(b) = std::fs::read(path) else { return };
    if b.len() < 28 { return; }
    let u32le = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    if u32le(0) != 0xCAFE_DADA { return; }
    let table_length = u32le(16) as usize;
    let locations_size = u32le(20) as usize;
    let header = 28;
    let offsets = header + table_length * 4; // skip redirect table (table_length × i32)
    let locations = offsets + table_length * 4;
    let strings = locations + locations_size;
    if strings > b.len() { return; }
    // A jimage string is NUL-terminated modified-UTF8 at `strings + off` (off 0 = empty).
    let read_str = |off: usize| -> &str {
        if off == 0 { return ""; }
        let start = strings + off;
        let mut e = start;
        while e < b.len() && b[e] != 0 { e += 1; }
        std::str::from_utf8(&b[start..e]).unwrap_or("")
    };
    // Decode an ImageLocation attribute stream into (module, parent, base, extension) string offsets.
    let decode = |mut p: usize| -> (usize, usize, usize, usize) {
        let (mut m, mut par, mut base, mut ext) = (0usize, 0usize, 0usize, 0usize);
        while p < b.len() {
            let byte = b[p];
            p += 1;
            let kind = byte >> 3;
            if kind == 0 { break; } // ATTRIBUTE_END
            let len = ((byte & 0x7) + 1) as usize;
            let mut v = 0usize;
            for _ in 0..len {
                if p >= b.len() { break; }
                v = (v << 8) | b[p] as usize;
                p += 1;
            }
            match kind {
                1 => m = v,    // MODULE
                2 => par = v,  // PARENT (package, '/'-separated)
                3 => base = v, // BASE (simple file name, incl. extension separator handling below)
                4 => ext = v,  // EXTENSION
                _ => {}        // OFFSET/COMPRESSED/UNCOMPRESSED — content attrs, unused for the index
            }
        }
        (m, par, base, ext)
    };
    for i in 0..table_length {
        let loc_off = u32le(offsets + i * 4) as usize;
        if loc_off == 0 { continue; }
        let (m, par, base, ext) = decode(locations + loc_off);
        // Index java module classes (`java.base`, `java.*`); skip the JDK's own `jdk.*`/`sun.*`
        // implementation modules' resources only by what they expose by name + ambiguity rules.
        if read_str(ext) != "class" { continue; }
        let parent = read_str(par);
        if parent.is_empty() { continue; }
        let internal = format!("{parent}/{}", read_str(base));
        let _ = m;
        register_class_name(&internal, idx, ambiguous);
    }
}
