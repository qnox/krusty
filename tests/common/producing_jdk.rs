//! The identity of the JDK that runs the reference toolchain.
//!
//! The persistent kotlinc server and javac both run on the JDK [`super::java_home`] selects
//! (`KRUSTY_REF_JAVA_HOME`, then `JAVA_HOME`). javac stamps its own feature release into every class
//! it emits (the harness passes no `--release`), and a full-JDK Kotlin compile resolves against that
//! JDK's `lib/modules`, so any cached product of that toolchain is valid only for the installation
//! that produced it. The identity is read from the installation's own files, never its path alone:
//! an in-place upgrade or a retargeted symlink keeps the path while changing the JDK. Reading two
//! small file records keeps the check free of a JVM start.

use std::path::Path;
use std::time::UNIX_EPOCH;

/// The key for a cache of driver classes the javac at `java_home` compiles once and its JVM loads:
/// the installation's identity, so the same JDK reached through another path reuses the classes and
/// a retargeted path recompiles them. A home that cannot be identified keys on its path, as before
/// identities existed; such a home has no usable javac anyway.
pub fn driver_cache_key(java_home: &str) -> Vec<u8> {
    jdk_identity(Path::new(java_home)).unwrap_or_else(|_| java_home.as_bytes().to_vec())
}

/// The identity of the JDK installed at `java_home`: its `release` record (vendor, version, and
/// module list) and the size and modification time of its `lib/modules` image, which holds both the
/// JDK class library a Kotlin compile resolves against and javac itself.
pub fn jdk_identity(java_home: &Path) -> Result<Vec<u8>, String> {
    let modules = java_home.join("lib").join("modules");
    let image = std::fs::metadata(&modules).map_err(|error| {
        format!(
            "cannot identify the reference JDK: {}: {error}",
            modules.display()
        )
    })?;
    let modified = image
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .ok_or_else(|| {
            format!(
                "cannot identify the reference JDK: {} has no modification time",
                modules.display()
            )
        })?;
    let release_path = java_home.join("release");
    let release = match std::fs::read(&release_path) {
        Ok(release) => release,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(format!(
                "cannot identify the reference JDK: {}: {error}",
                release_path.display()
            ))
        }
    };
    let mut identity = Vec::with_capacity(release.len() + 32);
    identity.extend_from_slice(&(release.len() as u64).to_le_bytes());
    identity.extend_from_slice(&release);
    identity.extend_from_slice(&image.len().to_le_bytes());
    identity.extend_from_slice(&modified.as_nanos().to_le_bytes());
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_jdk(root: &Path, release: &str, modules: &[u8]) {
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("release"), release).unwrap();
        std::fs::write(root.join("lib").join("modules"), modules).unwrap();
    }

    #[test]
    fn a_jdk_replaced_at_the_same_path_changes_its_identity() {
        let root = std::env::temp_dir().join(format!(
            "krusty_producing_jdk_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        fake_jdk(&root, "JAVA_VERSION=\"21.0.12\"\n", b"jimage-21");
        let first = jdk_identity(&root).expect("identify the first JDK");
        assert_eq!(
            jdk_identity(&root),
            Ok(first.clone()),
            "an unchanged JDK keeps its identity"
        );

        fake_jdk(&root, "JAVA_VERSION=\"25.0.4\"\n", b"jimage-21");
        let upgraded_release = jdk_identity(&root).expect("identify the upgraded JDK");
        assert_ne!(upgraded_release, first, "a new release record is a new JDK");

        fake_jdk(&root, "JAVA_VERSION=\"21.0.12\"\n", b"jimage-21 rebuilt");
        assert_ne!(
            jdk_identity(&root).expect("identify the rebuilt JDK"),
            first,
            "a rebuilt module image under the same release record is a new JDK"
        );

        let home = root.to_str().expect("UTF-8 temp path");
        assert_eq!(driver_cache_key(home), jdk_identity(&root).unwrap());

        std::fs::remove_file(root.join("lib").join("modules")).unwrap();
        assert!(
            jdk_identity(&root).is_err(),
            "a home without a module image cannot be identified"
        );
        assert_eq!(driver_cache_key(home), home.as_bytes());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
