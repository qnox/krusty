//! Classpath: resolve an internal class name (e.g. `util/Calc`) to its `ClassInfo` from either a
//! directory of loose `.class` files **or a `.jar`** (Java/Kotlin library support). Results are
//! cached. jar entries are read on demand (DEFLATE via the `zip` crate).

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

#[derive(Default)]
pub struct Classpath {
    entries: Vec<Entry>,
    cache: RefCell<HashMap<String, Option<ClassInfo>>>,
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
        Classpath { entries, cache: RefCell::new(HashMap::new()) }
    }

    pub fn empty() -> Classpath {
        Classpath::default()
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
}

fn read_jar_entry(jar: &Path, name: &str) -> Option<Vec<u8>> {
    let f = File::open(jar).ok()?;
    let mut archive = zip::ZipArchive::new(f).ok()?;
    let mut entry = archive.by_name(name).ok()?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}
