//! Classpath companion-object resolution: a bare reference to a classpath class with a `companion
//! object` (`Json` → `Json.Default`, `Random` → `Random.Default`) resolves to the companion INSTANCE.
//! This tests the library-layer detection (`LibraryType::companion_object`) — the substrate the
//! resolver/lowering use to emit `getstatic C.field:LcompanionType;` for such a bare reference.

use super::common;

use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::symbol_source::SymbolSource;
use krusty::types::Ty;
use std::rc::Rc;

#[test]
fn classpath_class_companion_object_is_detected() {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar located");
        return;
    };
    let cp = Rc::new(Classpath::new(vec![stdlib]));
    let libs = JvmLibraries::new(cp);

    // `kotlin.random.Random` has a companion object `Default` — a `public static final
    // kotlin/random/Random$Default Default` field on `Random` (the same shape as
    // `kotlinx.serialization.json.Json.Default`).
    let random = libs
        .resolve_type("kotlin/random/Random")
        .expect("kotlin/random/Random resolves on the stdlib classpath");
    let random_companion = random
        .companion_object
        .clone()
        .map(|(field, ty)| (field, ty.render()));
    assert_eq!(
        random_companion,
        Some((
            "Default".to_string(),
            "kotlin/random/Random$Default".to_string()
        )),
        "Random's companion-object instance field should be detected"
    );

    // A class without a companion object has none — the detection must not false-positive on an
    // unrelated static field.
    let pair = libs
        .resolve_type("kotlin/Pair")
        .expect("kotlin/Pair resolves");
    assert!(
        pair.companion_object.is_none(),
        "kotlin/Pair has no companion object"
    );

    // The companion-INSTANCE method-call path: `Random.nextInt(n)` = `Random.Default.nextInt(n)`.
    // The companion's type (`Random$Default`) must resolve an instance method `nextInt(Int)`
    // (inherited from `Random` via the supertype walk) — what the checker/lowering use to lower the
    // call as `getstatic Random.Default; invokevirtual nextInt`.
    let (_, cty) = random
        .companion_object
        .expect("Random has a companion object");
    use krusty::symbol_resolver::{SymRecv, Symbol};
    let nextint = krusty::symbol_resolver::SymbolResolver::new(&libs)
        .resolve_symbol(SymRecv::Type(&cty.render()), "nextInt", &[Ty::Int], &[])
        .and_then(Symbol::instance);
    assert!(
        nextint.is_some(),
        "nextInt(Int) resolves as an instance method on the companion type {}",
        cty.render()
    );
}
