//! A typealias constructor in an INFERRED declaration.
//!
//! `val p = AliasedCell(MyClass())` is an unresolved reference unless the alias's expansion is
//! installed before declaration inference runs. The deferred run types a declaration by running the
//! real checker over its body, and source alias expansions were registered AFTER that run — so the
//! same call resolved inside a function body and declined at the top level.
use super::common;

#[test]
fn a_generic_typealias_constructor_types_an_inferred_declaration() {
    const SRC: &str = "class Cell<T>(val x: T)\n\
        typealias AliasedCell<TT> = Cell<TT>\n\
        class MyClass\n\
        val p = AliasedCell(MyClass())\n\
        fun box(): String = if (p.x is MyClass) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "GenericTypeAliasCtorInference");
}

#[test]
fn a_non_generic_typealias_constructor_types_an_inferred_declaration() {
    const SRC: &str = "class Cell<T>(val x: T)\n\
        class MyClass\n\
        typealias Fixed = Cell<MyClass>\n\
        val p = Fixed(MyClass())\n\
        fun box(): String = if (p.x is MyClass) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "FixedTypeAliasCtorInference");
}
