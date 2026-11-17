//! A `crossinline` literal reaches its file's and class's private declarations through synthetic
//! `access$<name>` accessors, as kotlinc's synthetic-accessor lowering requires of every such
//! literal's body. That holds when a same-module call of the inline function is expanded in IR and
//! the literal's body is spliced back into its own declaring class: a private function, a callable
//! reference to one, a private member function, and a private property's backing field are each
//! reached through the accessor.

use super::common;

#[test]
fn an_expanded_crossinline_literal_reaches_private_declarations_through_accessors() {
    let source = "inline fun h(crossinline f: () -> Boolean) = f()\n\
        fun a() = h { pos(1) }\n\
        fun r() = h(::one)\n\
        private fun one() = pos(1)\n\
        private fun pos(i: Int) = i > 0\n\
        class K(private val x: Int) {\n\
        \x20   private fun mem() = x > 0\n\
        \x20   fun d() = h { mem() }\n\
        \x20   fun e() = h { x > 0 }\n\
        }\n";
    let classes =
        common::classes_against_kotlinc_module(&[("CrossinlineExpandedAccessor.kt", source)]);
    for class in ["CrossinlineExpandedAccessorKt", "K"] {
        let (reference, krusty) = classes.class_listing(class);
        assert_eq!(
            krusty, reference,
            "{class}: kotlinc's code and debug tables"
        );
    }
}
