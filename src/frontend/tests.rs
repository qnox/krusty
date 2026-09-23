use super::*;
use crate::diag::{Diagnostic, Span};
use crate::libraries::{
    CallSig, Callables, FnKind, FunctionInfo, FunctionSet, GenericSig, LibraryCallable,
    LibraryType, PropKind, PropertyInfo, PropertySet, ResolvedSymbols, TypeKind,
};
use crate::source::SourceInput;
use crate::types::{Ty, TypeName, TypeNameList, Visibility};

mod analysis;
mod retention;
mod streaming;

#[test]
fn anonymous_defaults_receive_enclosing_classifier_identities() {
    fn anonymous_name(source: &str, facade: &str) -> String {
        let mut diagnostics = crate::diag::DiagSink::new();
        let mut file = parse_source(source, &LangFeatures::new(), &mut diagnostics);
        assert!(!diagnostics.has_errors(), "{:#?}", diagnostics.diags);
        name_anonymous_classes(&mut file, facade);
        let declaration = *file
            .anonymous_object_classes
            .values()
            .next()
            .expect("source must contain an anonymous object");
        let crate::ast::Decl::Class(class) = file.decl(declaration) else {
            panic!("anonymous object must map to a classifier")
        };
        class.name.clone()
    }

    assert_eq!(
        anonymous_name(
            "open class FooA\nclass BarA(val foo: FooA? = object : FooA() {})",
            "AKt",
        ),
        "BarA$1",
    );
    assert_eq!(
        anonymous_name(
            "open class FooC\nclass BarC(val foo: FooC? = object : FooC() {})",
            "CKt",
        ),
        "BarC$1",
    );
}

/// Every anonymous object's invented name, in source order.
fn anonymous_names_in_source_order(source: &str, facade: &str) -> Vec<String> {
    let mut diagnostics = crate::diag::DiagSink::new();
    let mut file = parse_source(source, &LangFeatures::new(), &mut diagnostics);
    assert!(!diagnostics.has_errors(), "{:#?}", diagnostics.diags);
    name_anonymous_classes(&mut file, facade);
    let mut objects = file
        .anonymous_object_classes
        .iter()
        .map(|(construction, declaration)| {
            (file.expr_spans[construction.0 as usize].lo, *declaration)
        })
        .collect::<Vec<_>>();
    objects.sort_unstable_by_key(|(start, _)| *start);
    objects
        .into_iter()
        .map(|(_, declaration)| match file.decl(declaration) {
            crate::ast::Decl::Class(class) => class.name.clone(),
            _ => panic!("anonymous object must map to a classifier"),
        })
        .collect()
}

// The expected names below are the class files kotlinc 2.4.10 writes for the same sources.

#[test]
fn anonymous_objects_share_the_sequence_lambdas_references_and_delegates_number() {
    let source = r#"
interface I { fun v(): String }
fun run(f: () -> Unit) = f()
fun take(x: Any?) = x
fun f() {}
fun lambdaThenObject(): String {
    run { }
    take(object : I { override fun v() = "a" })
    val o = object : I { override fun v() = "b" }
    return o.v()
}
val top = object : I { override fun v() = "c" }
val lazyTop by lazy { object : I { override fun v() = "d" } }
suspend fun susp(): Any {
    run { }
    return object : I { override fun v() = "e" }
}
fun foo(x: Int): Any = object : I { override fun v() = "f" }
fun Foo(): Any = object : I { override fun v() = "g" }
fun foo(s: String): Any { run(::f); return object : I { override fun v() = "h" } }
fun localDelegates(): Any {
    var v by Delegates.observable(1) { _, _, _ -> }
    take(v)
    return object : I { override fun v() = "i" }
}
"#;
    assert_eq!(
        anonymous_names_in_source_order(source, "NKt"),
        [
            // The lambda before it holds `$1`; a local variable names its initializer's chain.
            "NKt$lambdaThenObject$2",
            "NKt$lambdaThenObject$o$1",
            "NKt$top$1",
            // `$1` is reserved for the delegated property, `$2` is the `lazy` lambda.
            "NKt$lazyTop$2$1",
            // The continuation holds `$1`, the lambda `$2`.
            "NKt$susp$3",
            // Overloads and names differing only in case share one sequence.
            "NKt$foo$1",
            "NKt$Foo$2",
            "NKt$foo$4",
            // A local delegated `var` takes the reservation, then its getter and setter.
            "NKt$localDelegates$1",
        ],
    );
}

#[test]
fn anonymous_objects_in_classifiers_follow_kotlinc_member_chains() {
    let source = r#"
interface I { fun v(): String }
fun run(f: () -> Unit) = f()
fun take(x: Any?) = x
fun f() {}
class C(val p: Any = object : I { override fun v() = "a" }) {
    init { take(object : I { override fun v() = "b" }) }
    val q = object : I { override fun v() = "c" }
    constructor(x: Int) : this(object : I { override fun v() = "d" })
    init { run { }; take(object : I { override fun v() = "e" }) }
    fun m() { run(::f); take(object : I { override fun v() = "f" }) }
    suspend fun s() { take(object : I { override fun v() = "g" }) }
}
class M {
    companion object {
        val c = object : I { override fun v() = "h" }
        fun cm(): Any { take { }; return object : I { override fun v() = "i" } }
    }
    class Nested { init { take(object : I { override fun v() = "j" }) } }
}
class D(i: I) : I by object : I { override fun v() = "k" }
enum class E(val x: Any) {
    A(object : I { override fun v() = "l" }),
    B(listOf({ 2 })) { override fun t() = object : I { override fun v() = "m" } },
    C(object : I { override fun v() = "n" });
    open fun t(): Any = 0
}
"#;
    assert_eq!(
        anonymous_names_in_source_order(source, "NKt"),
        [
            // Constructors and `init` blocks add no name: they number the class's own chain, in
            // source order, around the property that names its own.
            "C$1",
            "C$2",
            "C$q$1",
            "C$3",
            "C$5",
            "C$m$2",
            // The continuation of `s` holds `$1`.
            "C$s$2",
            "M$Companion$c$1",
            "M$Companion$cm$2",
            "M$Nested$1",
            // The `$$delegate_0` field adds no name.
            "D$1",
            "E$1",
            // A bodied entry's arguments are numbered in its own class, so `C`'s object is `$2`.
            "E$B$t$1",
            "E$2",
        ],
    );
}

#[test]
fn anonymous_objects_in_local_scopes_follow_kotlinc_chains() {
    let source = r#"
interface I { fun v(): String }
fun take(x: Any?) = x
open class Base(val x: Any)
fun superArgs(): Any = object : Base(listOf({ 1 })) {
    fun inner() = object : I { override fun v() = "a" }
}
fun locals(): Any {
    fun local(): Any = object : I { override fun v() = "b" }
    val d by lazy { 1 }
    take(d)
    val (a, b) = Pair(object : I { override fun v() = "c" }, 1)
    for (x in listOf(object : I { override fun v() = "d" })) take(x)
    when (val s = object : I { override fun v() = "e" }) { else -> take(s) }
    return local()
}
"#;
    assert_eq!(
        anonymous_names_in_source_order(source, "NKt"),
        [
            // The super-constructor lambda is numbered after the object, in the outer chain.
            "NKt$superArgs$1",
            "NKt$superArgs$1$inner$1",
            "NKt$locals$local$1",
            // Destructuring containers and loop iterators are temporaries: they add no name.
            "NKt$locals$1",
            "NKt$locals$2",
            "NKt$locals$s$1",
        ],
    );
}

#[test]
fn suspend_continuations_hold_their_place_in_the_shared_sequence() {
    let source = r#"
suspend fun g(x: Int) {}
suspend fun G(s: String) {}
fun take(x: Any?) = x
class K { suspend fun g() { take(object {}) } }
"#;
    let mut diagnostics = crate::diag::DiagSink::new();
    let mut file = parse_source(source, &LangFeatures::new(), &mut diagnostics);
    assert!(!diagnostics.has_errors(), "{:#?}", diagnostics.diags);
    name_anonymous_classes(&mut file, "NKt");
    let mut ordinals = file
        .suspend_continuation_ordinals
        .iter()
        .map(|(function, ordinal)| (format!("{function:?}"), *ordinal))
        .collect::<Vec<_>>();
    ordinals.sort_unstable_by_key(|(_, ordinal)| *ordinal);
    assert_eq!(
        ordinals
            .iter()
            .map(|(_, ordinal)| *ordinal)
            .collect::<Vec<_>>(),
        [1, 1, 2],
        "{ordinals:?}"
    );
}

#[test]
fn file_facade_names_follow_kotlinc_package_part_rules() {
    // The class files kotlinc 2.4.10 writes for these file names.
    for (stem, facade) in [
        ("box", "BoxKt"),
        ("1", "_1Kt"),
        (
            "32defaultParametersInSuspend",
            "_32defaultParametersInSuspendKt",
        ),
        ("a-b", "A_bKt"),
        ("x.y", "X_yKt"),
        ("q$r", "Q_rKt"),
        ("_u", "_uKt"),
    ] {
        assert_eq!(file_facade_simple_name(stem), facade, "{stem}");
    }
}
