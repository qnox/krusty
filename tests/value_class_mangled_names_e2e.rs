//! kotlinc's value-class JVM names follow two separate facts about each parameter type, both read
//! through the type's erased upper bound: whether it *requires* mangling (a value class other than
//! `kotlin.Result`) and how it is *spelled* in the hashed signature (`L<fqName>[?];` for every value
//! class, `kotlin.Result` included, `_` otherwise).
//!
//! krusty conflated the two (a `Result` parameter hashed as `_`), decided an accessor's and a
//! constructor's value-class-ness from the declared classifier alone (a `T : Z?` property kept a
//! plain `getZ`, a `Result` constructor parameter hid its constructor), and named a bridge after the
//! override's substituted parameters instead of the supertype member's declared ones.
use super::common;

/// Each listed class is written byte for byte as kotlinc writes it.
fn assert_classes_match_kotlinc(file: &str, source: &str, classes: &[&str]) {
    let built = common::classes_against_kotlinc_module(&[(file, source)]);
    let mismatches = classes
        .iter()
        .filter_map(|&class| {
            let reference = built
                .reference
                .get(class)
                .unwrap_or_else(|| panic!("kotlinc writes {class}"));
            let ours = built
                .krusty
                .get(class)
                .unwrap_or_else(|| panic!("krusty writes {class}"));
            (reference != ours).then(|| common::exact_class_difference(class, reference, ours))
        })
        .collect::<Vec<_>>();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

#[test]
fn a_result_parameter_is_hashed_once_another_parameter_or_the_result_requires_mangling() {
    // `mixed` hashes "Lkotlin.Result;LIC;" (`mixed-bIshmZE`), `same` hashes
    // "Lkotlin.Result;:Lkotlin.Result;", `nullable` spells its `Result?` as `Lkotlin.Result?;`;
    // `onlyResult` requires nothing and keeps its plain name.
    const SOURCE: &str = "@JvmInline value class IC(val value: Any)\n\
        fun mixed(a: Result<Int>, ic: IC): Int = 1\n\
        fun onlyResult(a: Result<Int>): Int = 2\n\
        class Holder {\n\
        \x20   fun same(a: Result<Int>): Result<Int> = a\n\
        \x20   fun nullable(a: Result<Int>?, ic: IC): Result<Int> = a!!\n\
        }\n";
    assert_classes_match_kotlinc("ResultNames.kt", SOURCE, &["ResultNamesKt", "Holder"]);
}

#[test]
fn a_bridge_takes_the_supertype_members_name_whatever_the_override_substitutes() {
    // `I<T>.foo(x: T): IC` is `foo-<hash of "_:LIC;">` on `I`, and `C`'s bridge answers to that
    // name while `C.foo(x: IC2)` hashes its own parameter. The bridge receives the boxed `IC2` from
    // a generic call and unboxes it for the override.
    const SOURCE: &str = "@JvmInline value class IC(val v: Int)\n\
        @JvmInline value class IC2(val s: String)\n\
        interface I<T> { fun foo(x: T): IC }\n\
        class C : I<IC2> { override fun foo(x: IC2): IC = IC(x.s.length) }\n\
        interface J<T> { fun bar(x: T): Result<Int> }\n\
        class D : J<Result<Int>> { override fun bar(x: Result<Int>): Result<Int> = x }\n\
        class E : J<IC2> { override fun bar(x: IC2): Result<Int> = Result.success(x.s.length) }\n\
        fun viaI(i: I<IC2>): Int = i.foo(IC2(\"ab\")).v\n\
        fun viaJ(j: J<IC2>): Int = j.bar(IC2(\"abc\")).getOrThrow()\n\
        fun viaResult(j: J<Result<Int>>): Int = j.bar(Result.success(4)).getOrThrow()\n\
        fun box(): String =\n\
        \x20   if (viaI(C()) == 2 && viaJ(E()) == 3 && viaResult(D()) == 4) \"OK\" else \"FAIL\"\n";
    assert_classes_match_kotlinc("BridgeNames.kt", SOURCE, &["I", "C", "D", "E"]);
    common::expect_box_ok_with_stdlib(SOURCE, "BridgeNames");
}

#[test]
fn a_property_typed_by_a_value_class_bounded_type_parameter_mangles_its_accessors() {
    // `val z: T` with `T : Z1` is `getZ-<hash of ":LZ1;">`, `T : Z1?` hashes `:LZ1?;`, and the
    // setter of `var w: T` hashes its parameter, exactly as the bound itself would.
    const SOURCE: &str = "@JvmInline value class Z1(val x: Int)\n\
        @JvmInline value class Wrapped<T : Z1?>(val z: T)\n\
        class Holder<T : Z1>(val z: T, var w: T)\n\
        fun use(h: Holder<Z1>, w: Wrapped<Z1?>): Int {\n\
        \x20   h.w = h.z\n\
        \x20   return h.z.x + h.w.x + (w.z?.x ?: 0)\n\
        }\n";
    assert_classes_match_kotlinc(
        "BoundedAccessors.kt",
        SOURCE,
        &["Wrapped", "Holder", "BoundedAccessorsKt"],
    );
}

#[test]
fn a_constructor_is_hidden_exactly_when_a_parameter_requires_mangling() {
    // `Result` requires no mangling, so `ResultHolder` keeps a public constructor, which its
    // `@Metadata` records over the `Object` carrier; a parameter of type `T : Z1?` requires it
    // through its bound, so `Bounded` gets the private constructor and its
    // `DefaultConstructorMarker` accessor.
    const SOURCE: &str = "@JvmInline value class Z1(val x: Int)\n\
        class ResultHolder(r: Result<Int>) { val v: Int = r.getOrThrow() }\n\
        class Bounded<T : Z1?>(val z: T)\n\
        fun make(): Int = ResultHolder(Result.success(1)).v + (Bounded(Z1(2)).z?.x ?: 0)\n";
    assert_classes_match_kotlinc(
        "HiddenConstructors.kt",
        SOURCE,
        &["ResultHolder", "Bounded", "HiddenConstructorsKt"],
    );
}

/// A mangled function whose JVM descriptor a reader can rebuild from its declared types (every
/// value-class parameter nullable and boxed) still records its physical name in `@Metadata`, with
/// no descriptor: `JvmMethodSignature(name = "accept-6FUe0Y4")`. krusty recorded a name only beside
/// a descriptor, so the facade's metadata lost the mangled name.
const NAME_ONLY_SIGNATURE: &str = r#"@JvmInline
value class NX(val x: String?)

@JvmInline
value class NY(val y: Number?)

fun accept(x: NX?, y: NY?): String = if (x == null && y?.y == 1) "OK" else "fail"

fun box(): String = accept(null, NY(1))
"#;

#[test]
fn a_derivable_mangled_signature_records_only_its_name() {
    common::assert_classes_identical_to_kotlinc(
        "NameOnlySignature",
        NAME_ONLY_SIGNATURE,
        &["NameOnlySignatureKt"],
    );
}

#[test]
fn name_only_signature_runs() {
    common::expect_box_ok_with_stdlib(NAME_ONLY_SIGNATURE, "NameOnlySignature");
}

