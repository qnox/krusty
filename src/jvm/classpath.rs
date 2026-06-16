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
    types: RefCell<Option<TypeIndex>>,
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
                if is_archive {
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
    /// Result is cached after the first call.
    pub fn scan_types(&self) -> TypeIndex {
        if let Some(idx) = self.types.borrow().as_ref() {
            return idx.clone();
        }
        let mut idx = TypeIndex::default();
        // Track ambiguous simple names so we can remove them.
        let mut ambiguous: std::collections::HashSet<String> = std::collections::HashSet::new();

        for e in &self.entries {
            match e {
                Entry::Dir(d) => scan_types_dir(d, &mut idx, &mut ambiguous),
                Entry::Jar(j) => scan_types_jar(j, &mut idx, &mut ambiguous),
            }
        }
        // Remove ambiguous simple names that map to multiple internal names.
        for name in &ambiguous {
            idx.class_names.remove(name.as_str());
        }
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

fn process_class_bytes_for_types(bytes: &[u8], idx: &mut TypeIndex, ambiguous: &mut std::collections::HashSet<String>) {
    let Ok(ci) = parse_class(bytes) else { return };
    let internal = ci.this_class.clone();
    if internal.is_empty() { return; }
    // Simple name = last segment of internal name.
    let simple = internal.rsplit('/').next().unwrap_or(&internal).to_string();
    // Skip synthetic/anonymous classes (contain '$').
    if simple.contains('$') { return; }

    // Register class name (detect ambiguity).
    if let Some(existing) = idx.class_names.get(&simple) {
        if existing != &internal {
            ambiguous.insert(simple.clone());
        }
    } else if !ambiguous.contains(&simple) {
        idx.class_names.insert(simple.clone(), internal.clone());
    }

    // Type aliases from *TypeAliasesKt.class files: parse the $annotations() method names
    // to get alias names, then correlate with d2 to find the target descriptors.
    if simple.ends_with("TypeAliasesKt") && !ci.kotlin_d2.is_empty() {
        let alias_names: Vec<String> = ci.methods.iter()
            .filter(|m| m.name.ends_with("$annotations"))
            .map(|m| m.name.trim_end_matches("$annotations").to_string())
            .collect();
        // In d2, alias name and its JVM descriptor appear as consecutive strings:
        // name → "Lsome/Target;" (a JVM class descriptor).
        let d2 = &ci.kotlin_d2;
        for alias in &alias_names {
            // Find the alias name in d2, then look for the next JVM descriptor.
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
}

/// Convert a JVM class descriptor `Lsome/Class;` to internal name `some/Class`.
fn desc_to_internal(desc: &str) -> Option<String> {
    let s = desc.strip_prefix('L')?.strip_suffix(';')?;
    if s.is_empty() { return None; }
    Some(s.to_string())
}

fn scan_types_dir(dir: &Path, idx: &mut TypeIndex, ambiguous: &mut std::collections::HashSet<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            scan_types_dir(&p, idx, ambiguous);
        } else if p.extension().map_or(false, |x| x == "class") {
            if let Ok(b) = std::fs::read(&p) {
                process_class_bytes_for_types(&b, idx, ambiguous);
            }
        }
    }
}

fn scan_types_jar(jar: &Path, idx: &mut TypeIndex, ambiguous: &mut std::collections::HashSet<String>) {
    let Ok(f) = File::open(jar) else { return };
    let Ok(mut archive) = zip::ZipArchive::new(f) else { return };
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else { continue };
        if !entry.name().ends_with(".class") { continue; }
        let mut buf = Vec::new();
        if entry.read_to_end(&mut buf).is_ok() {
            process_class_bytes_for_types(&buf, idx, ambiguous);
        }
    }
}
