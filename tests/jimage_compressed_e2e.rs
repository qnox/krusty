//! Reading JDK classes from the `lib/modules` jimage must work whether the image stores resources
//! UNCOMPRESSED (Temurin) or zlib-COMPRESSED (the JetBrains Runtime, and `jlink --compress` images).
//! A compressed resource is wrapped in a 29-byte `CompressedResourceHeader` (magic `0xCAFEFAFA`) before
//! a zlib Deflate stream; krusty inflates it transparently. Without that, every JDK type is unresolvable
//! (e.g. `kotlin/collections/List` → `java/util/List` can't be found, so `for (x in listOf(...))` breaks).

mod common;

use krusty::jvm::classpath::Classpath;
use std::path::PathBuf;

/// The JDK jimage of the runner JVM, or `None` to skip (no `JAVA_HOME`/`lib/modules`).
fn jimage() -> Option<PathBuf> {
    let jh = common::java_home()?;
    let p = PathBuf::from(format!("{jh}/lib/modules"));
    p.is_file().then_some(p)
}

#[test]
fn core_jdk_classes_resolve_from_jimage() {
    let Some(img) = jimage() else {
        eprintln!("skip: set JAVA_HOME to a JDK with lib/modules");
        return;
    };
    let cp = Classpath::new(vec![img]);
    // These exist in every JDK's `java.base`. Failure here means the jimage reader can't read this
    // image's resources at all — the compressed-resource case this test pins.
    for name in [
        "java/lang/Object",
        "java/lang/String",
        "java/util/List",
        "java/util/ArrayList",
        "java/lang/Iterable",
    ] {
        assert!(
            cp.find(name).is_some(),
            "{name} must resolve from the JDK jimage (compressed or uncompressed)"
        );
    }
}
