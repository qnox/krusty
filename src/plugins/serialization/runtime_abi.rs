/// The `kotlinx.serialization` runtime ABI the generated code must match. The synthesized member
/// shape changed across releases, so the plugin emits *per target version* — exactly as krusty
/// itself is pinned to a kotlinc version. A mismatch between generated code and the linked runtime
/// is a runtime linkage error, so this is not cosmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SerializationAbi {
    /// core < 1.6: the per-class write helper is the unmangled `write$Self`.
    V1_0,
    /// core >= 1.6 (Kotlin >= 1.8.20): the helper is module-mangled `write$Self$<module>` to avoid
    /// cross-module name clashes.
    #[default]
    V1_6Plus,
}

impl SerializationAbi {
    /// Pick the ABI from a `kotlinx-serialization-core` version string (`"1.8.1"`, `"1.5.0"`).
    /// krusty derives this from the runtime jar on `-classpath`, so the same inputs kotlinc gets
    /// select the same codegen. `< 1.6` → `V1_0`, else `V1_6Plus`.
    pub fn from_core_version(version: &str) -> SerializationAbi {
        let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        let major = parts.next().unwrap_or(0);
        let minor = parts.next().unwrap_or(0);
        if (major, minor) < (1, 6) {
            SerializationAbi::V1_0
        } else {
            SerializationAbi::V1_6Plus
        }
    }

    /// Detect the ABI from a `-classpath` jar list by finding the `kotlinx-serialization-core[-jvm]`
    /// jar and reading its version. Returns `None` if the runtime isn't on the classpath (then the
    /// `@Serializable` annotation itself wouldn't resolve — a user error kotlinc also reports).
    pub fn from_classpath(cp_jars: &[String]) -> Option<SerializationAbi> {
        cp_jars
            .iter()
            .filter_map(|j| {
                let name = j.rsplit('/').next().unwrap_or(j);
                let stem = name.strip_suffix(".jar")?;
                let rest = stem.strip_prefix("kotlinx-serialization-core")?;
                let rest = rest.strip_prefix("-jvm").unwrap_or(rest);
                let ver = rest.strip_prefix('-')?;
                Some(SerializationAbi::from_core_version(ver))
            })
            .next()
    }
}

#[cfg(test)]
mod tests {
    use super::SerializationAbi;

    #[test]
    fn detected_from_classpath_runtime() {
        assert_eq!(
            SerializationAbi::from_core_version("1.5.0"),
            SerializationAbi::V1_0
        );
        assert_eq!(
            SerializationAbi::from_core_version("1.6.0"),
            SerializationAbi::V1_6Plus
        );
        assert_eq!(
            SerializationAbi::from_core_version("1.8.1"),
            SerializationAbi::V1_6Plus
        );

        let cp = vec![
            "/x/kotlin-stdlib.jar".to_string(),
            "/x/kotlinx-serialization-core-jvm-1.8.1.jar".to_string(),
        ];
        assert_eq!(
            SerializationAbi::from_classpath(&cp),
            Some(SerializationAbi::V1_6Plus)
        );

        let old = vec!["/x/kotlinx-serialization-core-1.5.0.jar".to_string()];
        assert_eq!(
            SerializationAbi::from_classpath(&old),
            Some(SerializationAbi::V1_0)
        );

        assert_eq!(
            SerializationAbi::from_classpath(&["/x/kotlin-stdlib.jar".to_string()]),
            None
        );

        let many = vec![
            "/x/kotlinx-serialization-json-jvm-1.5.0.jar".to_string(),
            "/x/kotlinx-serialization-protobuf-jvm-1.5.0.jar".to_string(),
            "/x/kotlinx-serialization-core-jvm-1.8.1.jar".to_string(),
        ];
        assert_eq!(
            SerializationAbi::from_classpath(&many),
            Some(SerializationAbi::V1_6Plus)
        );

        let snap = vec!["/x/kotlinx-serialization-core-jvm-1.8.1-SNAPSHOT.jar".to_string()];
        assert_eq!(
            SerializationAbi::from_classpath(&snap),
            Some(SerializationAbi::V1_6Plus)
        );
    }
}
