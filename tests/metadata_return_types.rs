//! `@Metadata` return-type decoding recovers Kotlin's read-only/mutable distinction (`mutableListOf`
//! returns `MutableList`, `listOf` returns `List`) — which the JVM descriptor/`Signature` both erase to
//! `java/util/List`. This is the foundation for distinguishing read-only vs mutable collections.

use krusty::jvm::classpath::Classpath;
use krusty::jvm::metadata::package_function_return_types;
use std::path::PathBuf;

fn stdlib() -> Option<PathBuf> {
    let kc = std::env::var("KRUSTY_KOTLINC").ok().filter(|s| !s.is_empty())?;
    let jar = PathBuf::from(&kc)
        .parent()?
        .parent()?
        .join("lib/kotlin-stdlib.jar");
    jar.exists().then_some(jar)
}

#[test]
fn collection_factory_return_types_distinguish_mutable() {
    let Some(jar) = stdlib() else {
        eprintln!("skip: set KRUSTY_KOTLINC");
        return;
    };
    let cp = Classpath::new(vec![jar]);
    // `listOf`/`mutableListOf`/`emptyList` live in this CollectionsKt facade part.
    let ci = cp
        .find("kotlin/collections/CollectionsKt__CollectionsKt")
        .expect("CollectionsKt part on classpath");
    let rets = package_function_return_types(&ci);
    assert_eq!(
        rets.get("listOf").map(String::as_str),
        Some("kotlin/collections/List"),
        "listOf must decode to the read-only List from @Metadata"
    );
    assert_eq!(
        rets.get("mutableListOf").map(String::as_str),
        Some("kotlin/collections/MutableList"),
        "mutableListOf must decode to MutableList from @Metadata (JVM signature erases both to java/util/List)"
    );
    assert_eq!(
        rets.get("emptyList").map(String::as_str),
        Some("kotlin/collections/List")
    );
    // A type stored directly in the d2 string table (not a predefined) still resolves.
    assert_eq!(
        rets.get("arrayListOf").map(String::as_str),
        Some("java/util/ArrayList")
    );
}
