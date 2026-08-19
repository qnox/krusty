//! Declaration types the signature WALK cannot compute, answered by the resolution engine.
//!
//! Each shape here reached emission with a resolution answer in place of a type before the engine
//! was asked: `Ty::Error` or the not-determined marker, both of which erase to `java/lang/Object`
//! in a descriptor and to `<error>` in `@Metadata`. A wrong descriptor is not a diagnostic — it is
//! a class that loads and then throws — so every case runs on a real JVM rather than only checking.

use super::common;

/// An anonymous object's override with an EXPRESSION body: its return type is inferred, and the
/// override only overrides when the descriptor matches the interface's.
///
/// `resumeWith` returns `Unit` here, so the method must be `(Ljava/lang/Object;)V`. Published as an
/// undetermined answer it became `(Ljava/lang/Object;)Ljava/lang/Object;` — a different method, so
/// the class verified, loaded, and threw `AbstractMethodError` at the first call.
#[test]
fn anonymous_object_override_with_inferred_return_implements_the_interface() {
    common::expect_box_ok_with_stdlib(
        "interface Sink {\n\
         \x20 fun accept(value: Any?)\n\
         }\n\
         fun drive(sink: Sink): String { sink.accept(\"x\"); return \"OK\" }\n\
         fun box(): String = drive(object : Sink {\n\
         \x20 override fun accept(value: Any?) = check(value != null)\n\
         })\n",
        "engine-anon-override-return",
    );
}

/// A member extension whose expression body calls a LATER member extension on the same receiver.
///
/// The call is unqualified, so it reaches the bare-name seam, and the declaration it names is a
/// member of an enclosing `this` — which the module index does not carry. Left undetermined, the
/// first extension settled to `Unit` and `.length` on its result was an unresolved reference.
#[test]
fn member_extension_reads_a_later_member_extension() {
    common::expect_box_ok_with_stdlib(
        "class Item\n\
         class Mapper {\n\
         \x20 fun Item.first() = second()\n\
         \x20 fun Item.second() = \"OK\"\n\
         \x20 fun convert(item: Item): String = item.first()\n\
         }\n\
         fun box(): String = Mapper().convert(Item())\n",
        "engine-member-ext-chain",
    );
}

/// A member extension that infers `Unit`, which no table records as an answer.
///
/// The marker has to settle, and member extensions keep their own signature table: one the settle
/// pass did not visit. The marker reached the emitted descriptor of a real method.
#[test]
fn member_extension_inferring_unit_settles_before_emission() {
    common::expect_box_ok_with_stdlib(
        "class Holder {\n\
         \x20 operator fun String.invoke() = Unit\n\
         \x20 fun run(): String { \"x\"(); return \"OK\" }\n\
         }\n\
         fun box(): String = Holder().run()\n",
        "engine-member-ext-unit",
    );
}

/// A delegated property whose `getValue` is a module-level EXTENSION with an inferred return.
///
/// The read goes through a receiver, so it arrives at the member seam, but the declaration is
/// top-level and no owner chain contains it. Undetermined, the property's own type stayed a marker
/// and reached a JVM descriptor, where it was not a type at all.
///
/// Checked rather than RUN: lowering a top-level delegated property to a module-level `getValue`
/// is a separate, pre-existing gap (the whole `delegatedProperty/delegateToFinalProperty` corpus
/// family fails the same way on master). What this fixes is the property's TYPE, so that is what
/// is asserted.
#[test]
fn delegate_get_value_is_a_module_extension_with_an_inferred_return() {
    common::expect_front_end_ok_files_with_stdlib(
        &["class Source { val value = 1 }\n\
           val source = Source()\n\
           operator fun Int.getValue(thisRef: Any?, property: Any?) =\n\
           \x20 if (this == 1) \"OK\" else \"FAIL\"\n\
           val delegated by source.value\n\
           fun use(): Int = delegated.length\n"],
        "engine-delegate-module-extension",
    );
}

/// A generic EXTENSION property whose getter is a lambda mentioning the property's own formal.
///
/// The walk types the lambda's parameter as an error and the whole property as `(<error>) -> T`.
/// Registered with that shape it resolved at every use site and carried `<error>` into `@Metadata`.
#[test]
fn generic_extension_property_with_a_lambda_getter() {
    common::expect_box_ok_with_stdlib(
        "val <T : CharSequence> T.identity\n\
         \x20 get() = { value: T -> value }\n\
         fun box(): String = \"OK\".identity(\"OK\")\n",
        "engine-generic-extension-property",
    );
}

/// An INTERSECTION bound that names the parameter it bounds.
///
/// A member not found on the erasure is retried against the remaining bounds, and `T : Comparable<T>`
/// hands that retry its own receiver back. Unguarded it recursed until the stack was gone.
#[test]
fn intersection_bound_that_names_its_own_parameter() {
    common::expect_box_ok_with_stdlib(
        "fun <T> width(value: T): Int where T : CharSequence, T : Comparable<T> = value.length\n\
         fun box(): String = if (width(\"OK\") == 2) \"OK\" else \"FAIL\"\n",
        "engine-self-referential-bound",
    );
}

/// An OVERLOADED member spelling with an inferred return.
///
/// A demand that arrives with a name alone cannot choose between overloads, so the by-spelling
/// index drops them — and dropping them from the PUBLISH pass too left both settling to `Unit`.
/// The consequence was argument-order dependence: the same two files compiled in one order and
/// reported "return type mismatch: expected 'String', actual 'Unit'" in the other.
#[test]
fn overloaded_member_returns_do_not_depend_on_file_order() {
    let user = "fun box(): String = Holder().pick()\n";
    let declaration = "class Holder { fun pick() = \"OK\"; fun pick(n: Int) = n }\n";
    common::expect_front_end_ok_files_with_stdlib(
        &[user, declaration],
        "engine-overload-user-first",
    );
    common::expect_front_end_ok_files_with_stdlib(
        &[declaration, user],
        "engine-overload-declaration-first",
    );
}

/// A member EXTENSION and a plain member sharing a spelling, one of them with a DECLARED return.
///
/// Publishing an inferred return by spelling wrote it onto whichever overload the extension table
/// happened to hold first — replacing a declared `Int` with a `String` and leaving the declaration
/// that was actually asked about undetermined. Two wrong descriptors from one publish.
#[test]
fn inferred_return_does_not_overwrite_a_same_named_extension() {
    common::expect_box_ok_with_stdlib(
        "class Holder {\n\
         \x20 fun String.z(): Int = length\n\
         \x20 fun z() = \"OK\"\n\
         \x20 fun use(): Int = \"ab\".z() + z().length\n\
         }\n\
         fun box(): String = if (Holder().use() == 4) \"OK\" else \"FAIL\"\n",
        "engine-extension-overload-publish",
    );
}

/// A module-level extension answers a receiver demand ONLY when its receiver matches.
///
/// `provideDelegate` here is declared on `String`, so it says nothing about a `Wrapper` delegate.
/// Answered on spelling alone it retyped the property from the wrong declaration's result — a
/// wrong descriptor and wrong `@Metadata`, with no diagnostic anywhere.
#[test]
fn module_extension_demand_requires_a_matching_receiver() {
    common::expect_box_ok_with_stdlib(
        "class Wrapper<T>(val value: T) {\n\
         \x20 operator fun getValue(thisRef: Any?, property: Any?) = value\n\
         }\n\
         operator fun String.provideDelegate(thisRef: Any?, property: Any?) = Wrapper(1)\n\
         val held by Wrapper(\"OK\")\n\
         fun box(): String = held\n",
        "engine-module-extension-receiver",
    );
}

/// A member PROPERTY and a member FUNCTION at the same index in their own lists.
///
/// Both were addressed as "member N of this class", so `object D { val base = 1; fun f() = … }`
/// gave `base` and `f` the same memo key: a demand for one was answered with the other's type.
/// Silently, and only when the two happened to occupy the same position.
#[test]
fn a_property_and_a_method_at_the_same_index_are_different_declarations() {
    common::expect_box_ok_with_stdlib(
        "object Registry {\n\
         \x20 val base = 100\n\
         \x20 fun scaled(x: Int) = x * 2 + base\n\
         \x20 fun total(a: Int, b: Int) = a + b + base\n\
         }\n\
         fun apply1(f: (Int) -> Int, v: Int) = f(v)\n\
         fun apply2(f: (Int, Int) -> Int, a: Int, b: Int) = f(a, b)\n\
         fun box(): String =\n\
         \x20 if (apply1(Registry::scaled, 3) == 106 && apply2(Registry::total, 4, 5) == 109)\n\
         \x20 \"OK\" else \"FAIL\"\n",
        "engine-member-key-collision",
    );
}

/// A QUALIFIED call to a declaration whose return is not determined yet.
///
/// The receiver names the owner and the callee spells the declaration — the same pair a bare-name
/// demand needs. Without the seam the call's type stayed the marker and the function that owned it
/// settled to `Unit`, so every use of it reported a mismatch against the wrong type.
#[test]
fn a_qualified_call_demands_the_declaration_it_names() {
    common::expect_box_ok_with_stdlib(
        "fun String.head2() = if (length >= 2) subSequence(0, 2) else null\n\
         fun width(s: String) = s.head2()?.length\n\
         fun box(): String = if (width(\"123\") == 2) \"OK\" else \"FAIL\"\n",
        "engine-qualified-call-demand",
    );
}

/// The same through a SAFE call, which is that read guarded rather than a different one.
#[test]
fn a_safe_call_demands_the_declaration_it_names() {
    common::expect_box_ok_with_stdlib(
        "fun String.head2() = if (length >= 2) subSequence(0, 2) else null\n\
         fun matches(s: String?) = 2 == s?.head2()?.length\n\
         fun box(): String = if (matches(\"123\") && !matches(null)) \"OK\" else \"FAIL\"\n",
        "engine-safe-call-demand",
    );
}

/// A member read through the IMPLICIT receiver, where the member selection beats the scope binding.
///
/// Walking the receiver tower is correct — a nearer receiver's same-named property must win — but
/// the selection reads the symbol table, which still holds the marker while that member is being
/// determined. Undetermined is not a type to win with.
#[test]
fn an_implicit_receiver_read_demands_an_undetermined_member() {
    common::expect_box_ok_with_stdlib(
        "class Counter {\n\
         \x20 val step = 2\n\
         \x20 fun advance(n: Int) = n * step\n\
         }\n\
         fun box(): String = if (Counter().advance(3) == 6) \"OK\" else \"FAIL\"\n",
        "engine-implicit-receiver-demand",
    );
}

/// A call through the `invoke` CONVENTION, in both spellings.
///
/// `method(i)` and `method.invoke(i)` reach the same declaration, and neither spells `invoke` where
/// a bare-name or a qualified demand looks: the first names a value, the second a member of a class
/// that has no member of that name written at the call. Both ask for `invoke` on the callee's type.
#[test]
fn an_invoke_convention_call_demands_the_operator_it_reaches() {
    common::expect_box_ok_with_stdlib(
        "class Doubler { operator fun invoke(n: Int) = n * 2 }\n\
         fun direct(d: Doubler, n: Int) = d(n)\n\
         fun spelled(d: Doubler, n: Int) = d.invoke(n)\n\
         fun box(): String =\n\
         \x20 if (direct(Doubler(), 3) == 6 && spelled(Doubler(), 4) == 8) \"OK\" else \"FAIL\"\n",
        "engine-invoke-convention-demand",
    );
}

/// A MEMBER EXTENSION called on a receiver that is not the class declaring it.
///
/// `fun Outer.tag()` declared inside `Inner` is reached as `Outer(…).tag()`, so the call's receiver
/// names `Outer` — which declares nothing of that name. The declaring class is on the `this` tower,
/// so a qualified call falls back to it rather than giving up.
#[test]
fn a_member_extension_is_demanded_through_the_declaring_class() {
    common::expect_box_ok_with_stdlib(
        "class Outer(val value: String) {\n\
         \x20 inner class Inner {\n\
         \x20 \x20 fun Outer.tag() = value\n\
         \x20 }\n\
         }\n\
         fun Outer.Inner.read() = Outer(\"OK\").tag()\n\
         fun box(): String = Outer(\"FAIL\").Inner().read()\n",
        "engine-member-extension-through-declarer",
    );
}
