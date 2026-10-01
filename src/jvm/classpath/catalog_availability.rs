//! Availability of the exact classpath identity catalog.
//!
//! Semantic provider lookup is allowed to consume only the catalog snapshot. If an entry could not
//! be inventoried completely, initialization fails before resolution rather than retrying through a
//! physical classfile spelling.

use super::Classpath;

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
