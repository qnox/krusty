//! Provisioning of the pinned serialization runtime shared by JVM and KLIB differentials.

use std::path::PathBuf;
use std::sync::OnceLock;

const SERIALIZATION_VERSION: &str = "1.9.0";

fn provisioned(artifact: &str) -> PathBuf {
    krusty::toolchain::ensure_maven("org.jetbrains.kotlinx", artifact, SERIALIZATION_VERSION)
        .unwrap_or_else(|| {
            panic!(
            "could not provision {artifact}:{SERIALIZATION_VERSION}; serialization differentials \
             must not self-skip. Check network access or set KRUSTY_DEPS_CACHE."
        )
        })
}

/// The pinned kotlinx.serialization core and json runtimes.
pub(super) fn runtime_libraries() -> Vec<PathBuf> {
    static JARS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    JARS.get_or_init(|| {
        vec![
            provisioned("kotlinx-serialization-core-jvm"),
            provisioned("kotlinx-serialization-json-jvm"),
        ]
    })
    .clone()
}
