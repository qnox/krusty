//! Classpath: resolve an internal class name (e.g. `util/Calc`) to its `ClassInfo` by reading
//! `<dir>/<name>.class`. Backs Java interop — krust learns a callee's signatures from its compiled
//! `.class` instead of hardcoding them. (Loose `.class` dirs for now; jar/jimage support later.)

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::jvm::classreader::{parse_class, ClassInfo};

#[derive(Default)]
pub struct Classpath {
    dirs: Vec<PathBuf>,
    cache: RefCell<HashMap<String, Option<ClassInfo>>>,
}

impl Classpath {
    pub fn new(dirs: Vec<PathBuf>) -> Classpath {
        Classpath { dirs, cache: RefCell::new(HashMap::new()) }
    }
    pub fn empty() -> Classpath {
        Classpath::default()
    }

    pub fn find(&self, internal: &str) -> Option<ClassInfo> {
        if let Some(hit) = self.cache.borrow().get(internal) {
            return hit.clone();
        }
        let mut found = None;
        for d in &self.dirs {
            let p = d.join(format!("{internal}.class"));
            if let Ok(bytes) = std::fs::read(&p) {
                if let Ok(ci) = parse_class(&bytes) {
                    found = Some(ci);
                    break;
                }
            }
        }
        self.cache.borrow_mut().insert(internal.to_string(), found.clone());
        found
    }
}
