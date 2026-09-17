//! Common optional-expectation annotations absent from the JVM stdlib classfiles.
//!
//! Kotlin ships the common `expect` headers in the distribution KLIB next to `kotlin-stdlib.jar`.
//! Only annotation classifiers whose metadata carries `IS_EXPECT_CLASS` are imported here; platform
//! declarations in the same archive never enter the JVM symbol source.
//!
//! The container itself is read through [`crate::klib::KlibArchive`], which is target-independent:
//! this is the JVM backend's *use* of a klib, not its own klib reader.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::klib::KlibArchive;
use crate::libraries::{LibraryType, TypeKind};
use crate::types::{type_name, TypeName};

#[derive(Default)]
pub(super) struct CommonExpectationIndex {
    classifiers: HashMap<TypeName, Arc<LibraryType>>,
}

impl CommonExpectationIndex {
    pub(super) fn load(path: Option<PathBuf>) -> Arc<Self> {
        type SharedIndex = Arc<OnceLock<Arc<CommonExpectationIndex>>>;
        static CACHE: OnceLock<Mutex<HashMap<PathBuf, SharedIndex>>> = OnceLock::new();
        let Some(path) = path else {
            return Arc::new(Self::default());
        };
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let shared = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(path.clone())
            .or_insert_with(|| Arc::new(OnceLock::new()))
            .clone();
        shared.get_or_init(|| Arc::new(Self::read(&path))).clone()
    }

    pub(super) fn classifier(&self, internal: TypeName) -> Option<Arc<LibraryType>> {
        self.classifiers.get(&internal).cloned()
    }

    pub(super) fn contains(&self, internal: TypeName) -> bool {
        self.classifiers.contains_key(&internal)
    }

    fn read(path: &Path) -> Self {
        let Some(archive) = KlibArchive::open(path) else {
            return Self::default();
        };
        let mut classifiers = HashMap::new();
        for fragment in archive.package_fragments() {
            let Some(bytes) = archive.read(&fragment.entry) else {
                continue;
            };
            for (internal, declaration) in
                crate::metadata::reader::parse_package_fragment(&bytes).classes
            {
                if declaration.kind != TypeKind::Annotation || !declaration.is_expect {
                    continue;
                }
                let identity = type_name(&internal);
                classifiers
                    .entry(identity)
                    .or_insert_with(|| Arc::new(annotation_type(identity, declaration)));
            }
        }
        Self { classifiers }
    }
}

fn annotation_type(
    identity: crate::types::TypeName,
    declaration: crate::metadata::reader::BuiltinClass,
) -> LibraryType {
    let mut classifier = crate::metadata::reader::library_type::library_type(identity, declaration);
    // What is this path's rather than the declaration's: no JVM actual exists for an optional
    // expectation, so this platform erases the annotation after checking it.
    classifier.retention = Some("SOURCE".to_string());
    classifier
}
