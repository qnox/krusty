//! Remember one successful `.kotlin_builtins` validation on a complete classpath.
//!
//! The box harness reuses one `Classpath` per worker. Parsing each fragment is already cached;
//! repeating the package walk on every `JvmLibraries` construction is not.

use super::{BuiltinsLoadError, Classpath};

impl Classpath {
    pub(in crate::jvm) fn validate_builtins(
        &self,
    ) -> Result<(), std::sync::Arc<BuiltinsLoadError>> {
        if self.builtins_validated.get() {
            return Ok(());
        }
        let tree = self.package_tree();
        let mut packages = tree.builtins_packages().collect::<Vec<_>>();
        packages.sort_by(|left, right| left.path_cmp(*right));
        drop(tree);
        for package in packages {
            self.try_builtins_file_for_package(package)?;
        }
        if self.catalog_complete() {
            self.builtins_validated.set(true);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Classpath;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
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
    fn a_successful_builtins_validation_is_not_repeated() {
        let Some(jar) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let classpath = Classpath::new(vec![jar]);
        classpath
            .validate_builtins()
            .expect("stdlib builtins validate");
        assert!(classpath.builtins_validated.get());
        assert!(!classpath.builtins.borrow().is_empty());
        classpath.builtins.borrow_mut().clear();

        classpath
            .validate_builtins()
            .expect("remembered validation");
        assert!(
            classpath.builtins.borrow().is_empty(),
            "a second validation must not walk builtins packages again"
        );
    }

    #[test]
    fn a_failed_builtins_validation_is_retried() {
        let directory = temp_dir("builtins-retry");
        let package = directory.join("broken");
        std::fs::create_dir(&package).expect("create builtins package");
        std::fs::write(package.join("broken.kotlin_builtins"), [0, 0, 0, 0, 0x0a])
            .expect("write corrupt builtins");
        let classpath = Classpath::new(vec![directory.clone()]);

        let first = classpath
            .validate_builtins()
            .expect_err("corrupt builtins")
            .to_string();
        assert!(!classpath.builtins_validated.get());
        let second = classpath
            .validate_builtins()
            .expect_err("failure is not remembered")
            .to_string();
        assert_eq!(
            first,
            format!(
                "cannot decode broken/broken.kotlin_builtins from {}: truncated package-fragment string table length at byte 1",
                directory.display()
            )
        );
        assert_eq!(first, second);

        std::fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn an_incomplete_catalog_does_not_remember_builtins_validation() {
        let directory = temp_dir("builtins-incomplete");
        let jar = directory.join("broken.jar");
        std::fs::write(&jar, b"not a zip").expect("write broken jar");
        let classpath = Classpath::new(vec![jar]);

        classpath
            .validate_builtins()
            .expect("an unreadable entry with no selected builtins still initializes");
        assert!(
            !classpath.builtins_validated.get(),
            "an incomplete catalog must be probed again"
        );

        drop(classpath);
        std::fs::remove_dir_all(directory).expect("remove test directory");
    }
}
