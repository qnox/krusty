//! `Classpath::metadata_call_facts` aligns a classpath function's bytecode candidate to its
//! `@Metadata` SOURCE signature, reporting how many leading descriptor params are REAL (an extension
//! receiver + the source value params). Any params beyond that are synthetic trailing params the
//! descriptor appends (a `@Composable` method's `(Composer, int)`), which the resolver truncates so a
//! source-arity call matches.
//!
//! These tests pin the REGRESSION guard: a normal/`vararg`/overloaded stdlib function must report its
//! FULL descriptor arity (no truncation), so the resolver never drops a real param — an earlier draft
//! truncated `mutableListOf<Int>()`'s `vararg` array down to zero (an empty-arg sibling overload's empty
//! value-param list prefix-matched), emitting an `invokestatic` with too few args → `VerifyError`.

use krusty::jvm::classpath::Classpath;
use krusty::types::{Ty, TypeName};

use super::common;

fn kept(cp: &Classpath, name: &str, params: &[Ty]) -> Option<usize> {
    // The return only tiebreaks return-distinguished overloads; `kept_params` here is decided by params.
    cp.metadata_call_facts(
        "kotlin/collections/CollectionsKt",
        name,
        params,
        &Ty::obj("java/lang/Object"),
        false,
        &|_| None,
    )
    .kept_params
}

#[test]
fn vararg_factory_keeps_its_array_param() {
    let jar = common::stdlib_jar();
    let cp = Classpath::new(vec![jar]);
    // `mutableListOf(vararg elements: T): MutableList<T>` → descriptor `([Ljava/lang/Object;)…`. The
    // `vararg` is ONE source value param (the array), so the kept count must be 1 — NOT 0 (which would
    // drop the array and underflow the operand stack at the call).
    let params = vec![Ty::array(Ty::obj("java/lang/Object"))];
    let mutable_kept = kept(&cp, "mutableListOf", &params);
    assert_eq!(
        mutable_kept,
        Some(1),
        "the vararg overload's array param must be kept (no truncation), got {mutable_kept:?}"
    );
    assert_eq!(
        kept(&cp, "listOf", &params),
        Some(1),
        "listOf's vararg array param must be kept too"
    );
}

#[test]
fn empty_factory_keeps_zero_params() {
    let jar = common::stdlib_jar();
    let cp = Classpath::new(vec![jar]);
    // The no-arg `listOf(): List<T>` overload — descriptor `()…`, zero params — aligns at zero kept,
    // which equals its descriptor arity, so the resolver truncates nothing.
    assert_eq!(kept(&cp, "listOf", &[]), Some(0));
}

#[test]
fn suspend_fact_is_descriptor_aligned_across_mangled_and_same_named_overloads() {
    // Keep every declaration synthetic while retaining the collision that a name-wide suspend set
    // cannot represent: one source name denotes an ordinary method whose real parameter happens to
    // be `Continuation`, plus a value-class-mangled suspend overload and its `$default` synthetic.
    let Some(library) = common::compile_libs(
        "SuspendFactLibrary",
        &[(
            "SuspendOverloads.kt",
            "package fixtures\n\
import kotlin.coroutines.Continuation\n\
fun pick(c: Continuation<Unit>): String = \"plain\"\n\
suspend fun pick(value: UInt, suffix: String = \"!\"): String = \"$value$suffix\"\n",
        )],
    ) else {
        return;
    };
    let cp = Classpath::new(vec![library]);
    let owner = "fixtures/SuspendOverloadsKt";
    let class = cp.find(owner).expect("synthetic facade");
    let plain = class
        .methods
        .iter()
        .find(|method| method.name == "pick")
        .expect("ordinary overload");
    let mangled = class
        .methods
        .iter()
        .find(|method| method.name.starts_with("pick-") && !method.name.ends_with("$default"))
        .expect("mangled suspend overload");
    let default = class
        .methods
        .iter()
        .find(|method| method.name.ends_with("$default"))
        .expect("mangled suspend default synthetic");
    let continuation = Ty::obj("kotlin/coroutines/Continuation");
    let underlying = |name: TypeName| name.matches("kotlin/UInt").then_some(Ty::Int);

    let plain_facts = cp.metadata_call_facts(
        owner,
        &plain.name,
        &[continuation],
        &Ty::String,
        false,
        &underlying,
    );
    assert_eq!(plain_facts.kept_params, Some(1));
    assert!(
        !plain_facts.suspend,
        "the mangled suspend sibling must not strip an ordinary Continuation parameter"
    );

    let suspend_facts = cp.metadata_call_facts(
        owner,
        &mangled.name,
        &[Ty::Int, Ty::String, continuation],
        &Ty::obj("java/lang/Object"),
        false,
        &underlying,
    );
    assert_eq!(suspend_facts.kept_params, Some(2));
    assert!(
        suspend_facts.suspend,
        "mangled JVM name must retain suspend"
    );

    // `$default` has no metadata entry. Its caller strips the suffix and the non-identifying
    // mask/marker ABI tail before this generic alignment; the remaining source-plus-Continuation
    // shape must select the same declaration, report two source parameters, and retain `suspend`.
    let default_facts = cp.metadata_call_facts(
        owner,
        default
            .name
            .strip_suffix("$default")
            .expect("selected default synthetic"),
        &[Ty::Int, Ty::String, continuation],
        &Ty::obj("java/lang/Object"),
        false,
        &underlying,
    );
    assert_eq!(default_facts.kept_params, Some(2));
    assert!(default_facts.suspend);
}
