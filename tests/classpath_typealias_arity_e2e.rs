//! A type alias expands to its target's OWN argument list, not the alias's.
//!
//! `typealias Lens<S, A> = PLens<S, S, A, A>` (arrow-optics' shape) declares two parameters for a
//! four-parameter target. krusty recorded only the target CLASSIFIER, so a use of `Lens<S, A>`
//! expanded to `PLens<S, A>` — the alias's own arguments pasted onto the target — and every
//! declaration written against the alias mismatched its initializer:
//!
//! ```text
//! initializer type mismatch: expected 'PLens<FlexibleValue, String>',
//!                            actual 'PLens<FlexibleValue, FlexibleValue, String, String>'
//! ```
//!
//! That single divergence accounted for the corpus's entire initializer-mismatch cluster.
use super::common;

const LIB: &str = "package lib\n\
    class PBox<S, T, A, B>(val tag: String)\n\
    typealias Box<S, A> = PBox<S, S, A, A>\n\
    typealias Plain = PBox<String, String, Int, Int>\n\
    typealias Handler<T> = (T) -> String\n\
    fun makeBox(): PBox<String, String, Int, Int> = PBox(\"made\")\n\
    fun anyBox(): PBox<Any, Any, Any, Any> = PBox(\"any\")\n\
    fun handler(): (String) -> String = { it }\n";

/// The dependency is compiled by the REFERENCE compiler on purpose: the contract under test is
/// krusty CONSUMING kotlinc's `TypeAlias` metadata, which records the alias's own parameters and
/// its expanded type. krusty's own emitter does not write those yet (it emits the bare target), so
/// a krusty-built fixture would exercise the gap rather than the fix; that emission gap is tracked
/// separately. `None` when the toolchain is unavailable.
fn kotlinc_lib() -> Option<std::path::PathBuf> {
    // Compiled ONCE per test process and shared: the tests run in parallel, so building into a
    // per-tag directory would have each run wipe the classes the others are reading.
    static LIB_OUT: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    LIB_OUT
        .get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("krusty_taa_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).ok()?;
            let source = dir.join("Lib.kt");
            std::fs::write(&source, LIB).ok()?;
            let out = dir.join("classes");
            std::fs::create_dir_all(&out).ok()?;
            let (code, stderr) = common::kotlinc_compile(&[
                source.to_string_lossy().to_string(),
                "-d".to_string(),
                out.to_string_lossy().to_string(),
            ])?;
            assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
            Some(out)
        })
        .clone()
}

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let libout = kotlinc_lib()?;
    Some(common::expect_box_run(
        main,
        "Main",
        &[libout, stdlib],
        Some(jdk.as_path()),
    ))
}

#[test]
fn alias_expands_to_the_targets_argument_list() {
    // The alias substitutes its own two arguments into the target's four positions.
    const MAIN: &str = "import lib.Box\n\
        import lib.makeBox\n\
        val b: Box<String, Int> = makeBox()\n\
        fun box(): String = if (b.tag == \"made\") \"OK\" else \"fail:\" + b.tag\n";
    assert_eq!(
        run(MAIN).expect("alias expands to the target's arity"),
        "OK"
    );
}

#[test]
fn alias_without_parameters_still_expands() {
    // A parameterless alias for a parameterized target: every argument comes from the alias's own
    // right-hand side, so nothing is substituted.
    const MAIN: &str = "import lib.Plain\n\
        import lib.makeBox\n\
        val b: Plain = makeBox()\n\
        fun box(): String = if (b.tag == \"made\") \"OK\" else \"fail:\" + b.tag\n";
    assert_eq!(run(MAIN).expect("parameterless alias expands"), "OK");
}

#[test]
fn alias_is_usable_as_a_function_parameter_and_return() {
    const MAIN: &str = "import lib.Box\n\
        import lib.makeBox\n\
        fun pass(v: Box<String, Int>): Box<String, Int> = v\n\
        fun box(): String = if (pass(makeBox()).tag == \"made\") \"OK\" else \"fail\"\n";
    assert_eq!(run(MAIN).expect("alias in signature position"), "OK");
}

#[test]
fn alias_use_keeps_nullability_and_projections() {
    // The arguments are projected BEFORE substitution, so a star argument stays an existential
    // out-projection rather than becoming an invariant `Any?` in all four target positions — and a
    // nullable use of the alias stays nullable.
    const MAIN: &str = "import lib.Box\n\
        import lib.makeBox\n\
        import lib.anyBox\n\
        fun box(): String {\n\
        \x20   val nullable: Box<String, Int>? = makeBox()\n\
        \x20   val star: Box<*, *> = anyBox()\n\
        \x20   return if (nullable?.tag == \"made\" && star.tag == \"any\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("nullable and star uses of an alias"), "OK");
}

#[test]
fn alias_expands_inside_a_generic_declaration() {
    // The alias's own formal names (`S`, `A`) must not capture the use site's type parameters even
    // when they collide: substitution is simultaneous, so `C<S>`'s `S` stays the caller's.
    const MAIN: &str = "import lib.Box\n\
        import lib.makeBox\n\
        class Holder<S>(val boxed: Box<S, Int>)\n\
        fun box(): String {\n\
        \x20   val h = Holder<String>(makeBox())\n\
        \x20   return if (h.boxed.tag == \"made\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("alias inside a generic declaration"), "OK");
}

#[test]
fn function_typed_alias_expands_to_its_function_type() {
    // `typealias Handler<T> = (T) -> String` expands to a FUNCTION type, which carries no type
    // arguments — a template kept only for its arguments would drop it and leave a bare
    // `FunctionN` that cannot be invoked.
    const MAIN: &str = "import lib.Handler\n\
        import lib.handler\n\
        fun box(): String {\n\
        \x20   val h: Handler<String> = handler()\n\
        \x20   return if (h(\"x\") == \"x\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("function-typed alias"), "OK");
}

#[test]
fn a_same_named_local_class_keeps_its_own_shape() {
    // A star-imported alias must not lend its expansion to a same-named class the module declares:
    // the template applies only when its head IS the classifier that resolution selected.
    const MAIN: &str = "import lib.*\n\
        class Box<S, A>(val own: String)\n\
        fun box(): String {\n\
        \x20   val mine: Box<String, Int> = Box(\"mine\")\n\
        \x20   return if (mine.own == \"mine\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("local class wins over a star-imported alias"),
        "OK"
    );
}

#[test]
fn fully_qualified_use_expands_too() {
    // A fully-qualified use names the alias without importing it, so the spelling itself is the
    // only candidate the probe can try.
    const MAIN: &str = "fun box(): String {\n\
        \x20   val b: lib.Box<String, Int> = lib.makeBox()\n\
        \x20   return if (b.tag == \"made\") \"OK\" else \"fail:\" + b.tag\n\
        }\n";
    assert_eq!(run(MAIN).expect("fully-qualified alias use"), "OK");
}

#[test]
fn a_nested_class_shadowing_an_alias_keeps_its_own_shape() {
    // Name resolution has channels that outrank imports — an enclosing class's NESTED type among
    // them — and they are not visible when the alias index is built. Deciding whether the template
    // applies must therefore happen where the winning classifier is known, or a nested `Box` here
    // inherits `lib.Box`'s expansion and every use of it mismatches.
    const MAIN: &str = "import lib.*\n\
        class Outer {\n\
        \x20   class Box<S, A>(val own: String)\n\
        \x20   fun make(): Box<String, Int> = Box(\"mine\")\n\
        }\n\
        fun box(): String = Outer().make().own\n";
    assert_eq!(
        run(MAIN).expect("nested class shadows a star-imported alias"),
        "mine"
    );
}

#[test]
fn a_nested_class_shadowing_a_parameterless_alias_keeps_its_own_shape() {
    const MAIN: &str = "import lib.*\n\
        class Outer {\n\
        \x20   class Plain(val own: String)\n\
        \x20   fun make(): Plain = Plain(\"mine\")\n\
        }\n\
        fun box(): String = Outer().make().own\n";
    assert_eq!(
        run(MAIN).expect("nested class shadows a parameterless alias"),
        "mine"
    );
}

#[test]
fn a_same_named_class_keeps_its_shape_against_a_function_alias() {
    // A function-typed template has no classifier head, so a head-only guard would exempt it and
    // let a user's own `Handler` inherit `(T) -> String`.
    const MAIN: &str = "import lib.*\n\
        class Handler<T>(val own: String)\n\
        fun box(): String {\n\
        \x20   val mine: Handler<String> = Handler(\"mine\")\n\
        \x20   return mine.own\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("user class shadows a function-typed alias"),
        "mine"
    );
}

#[test]
fn a_user_typealias_keeps_its_own_target() {
    // A module's own `typealias` resolves to ITS target, so a classpath template of the same name
    // must not describe it. Here the names are distinct, pinning the template/target guard separately
    // from the same-spelling precedence case below.
    const MAIN: &str = "import lib.Box\n\
        import lib.makeBox\n\
        class Own(val v: String)\n\
        typealias Mine = Own\n\
        fun box(): String {\n\
        \x20   val m: Mine = Own(\"hi\")\n\
        \x20   val b: Box<String, Int> = makeBox()\n\
        \x20   return if (b.tag == \"made\") m.v else \"fail\"\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("user typealias keeps its own target"),
        "hi"
    );
}

#[test]
fn a_same_named_source_alias_stops_lower_import_lookup() {
    // The same-package alias is the winning classifier declaration. Its lack of classpath expansion
    // metadata is final; resolution must not continue into the lower star-import level and borrow
    // `lib.Box`'s unrelated four-slot template.
    const MAIN: &str = "import lib.*\n\
        class Own(val value: String)\n\
        typealias Box = Own\n\
        fun box(): String {\n\
        \x20   val own: Box = Own(\"OK\")\n\
        \x20   return own.value\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("same-package alias stops lower import lookup"),
        "OK"
    );
}

#[test]
fn invalid_alias_arity_is_a_frontend_error() {
    const MAIN: &str = "import lib.Box\nval invalid: Box<String>? = null\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let libout = kotlinc_lib().expect("kotlinc dependency");
    let diagnostics = common::front_end_diagnostics(MAIN, &[libout, stdlib], Some(jdk.as_path()));
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("wrong number of type arguments for type alias 'Box'")
                && diagnostic.contains("expected 2, found 1")
        }),
        "expected an alias-arity diagnostic, got {diagnostics:?}"
    );
}
