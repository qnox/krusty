//! Availability of the exact classpath identity catalog.
//!
//! Semantic provider lookup is allowed to consume only the catalog snapshot. If an entry could not
//! be inventoried completely, initialization fails before resolution rather than retrying through a
//! physical classfile spelling.

use super::Classpath;

/// A directory class file is part of the identity catalog when its bytes are a class. Invalid
/// Kotlin metadata is a load diagnostic for that declaration, not a missing inventory. A truncated
/// or non-class file means the entry cannot be inventoried.
pub(super) fn directory_class_is_inventoried(jp: &mut super::JarPackages, bytes: &[u8]) -> bool {
    let info = match crate::jvm::classreader::parse_class(bytes) {
        Ok(info) => info,
        Err(crate::jvm::classreader::ReadError::BadKotlinMetadata(_)) => return true,
        Err(_) => return false,
    };
    let meta = &info.meta;
    if meta.package_functions.is_empty()
        && meta.package_properties.is_empty()
        && meta.type_aliases.is_empty()
        && meta.multifile_parts.is_empty()
    {
        return true;
    }
    // The declared package, which `@JvmPackageName` divorces from the class file's directory.
    let package = info.declaring_package();
    let facade_id = crate::types::insert_type_name_in(&jp.names, info.this_class);
    jp.facades.insert(facade_id);
    let entry = jp.entry_mut_name(package);
    if !entry.facades.contains(&facade_id) {
        entry.facades.push(facade_id);
    }
    true
}

impl Classpath {
    pub(in crate::jvm) fn validate_catalog(
        &self,
    ) -> Result<(), crate::libraries::PlatformInitializationError> {
        let tree = self.package_tree();
        let Some(&entry) = tree.incomplete_entries.first() else {
            return Ok(());
        };
        let path = self
            .entries
            .get(entry)
            .map(|entry| entry.path().display().to_string())
            .unwrap_or_else(|| format!("classpath entry {entry}"));
        Err(crate::libraries::PlatformInitializationError {
            message: format!(
                "cannot inventory classpath entry {path}: package/class catalog is incomplete"
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_catalog_stops_provider_initialization_with_an_exact_diagnostic() {
        let directory = std::env::temp_dir().join(format!(
            "krusty-incomplete-catalog-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir(&directory).expect("create catalog fixture");
        let broken = directory.join("broken.jar");
        std::fs::write(&broken, b"not a zip archive").expect("write broken classpath entry");

        let result =
            crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![
                broken.clone(),
            ])));
        let error = match result {
            Ok(_) => panic!("an incomplete provider catalog must fail initialization"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            crate::libraries::PlatformInitializationError {
                message: format!(
                    "cannot inventory classpath entry {}: package/class catalog is incomplete",
                    broken.display()
                ),
            }
        );

        std::fs::remove_dir_all(directory).expect("remove catalog fixture");
    }
}
