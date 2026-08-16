//! A generic class whose CONSTRUCTOR takes a function over the class's own type parameters.
//!
//! Shape (generic file-backed repository): `class Store<ROOT, DOMAIN>(wrapper: Wrapper<ROOT>, empty:
//! ROOT, toDomain: (ROOT) -> DOMAIN, toRoot: (DOMAIN) -> ROOT)` constructed with lambdas. The
//! constructor's declared parameter shape is the lambda's expectation, so it must be SUBSTITUTED
//! with the type arguments already known at the call — from an explicit `Store<A, B>(…)`, from the
//! expected result, or from a sibling argument that pins `ROOT` — before the lambda body is checked.
//! Handing the lambda the raw `(ROOT) -> DOMAIN` types its parameter as an unbound type variable:
//! every member read on it fails ("unresolved reference"), the failed body contributes nothing back,
//! and the class collapses to `Store<Any, Any>` so every later call on the value cascades.
//!
//! The lambda's RESULT is the other direction of the same inference: `DOMAIN` is only ever visible
//! in the lambda's return position, so the checked body has to feed it back.
use super::common;

const STORE: &str = "package repro\n\
    class Wrapper<T>(val name: String)\n\
    class Store<ROOT, DOMAIN>(\n\
        private val wrapper: Wrapper<ROOT>,\n\
        private val empty: ROOT,\n\
        private val toDomain: (ROOT) -> DOMAIN,\n\
        private val toRoot: (DOMAIN) -> ROOT,\n\
    ) {\n\
        fun domain(): DOMAIN = toDomain(empty)\n\
        fun roundTrip(domain: DOMAIN): ROOT = toRoot(domain)\n\
        fun label(): String = wrapper.name\n\
    }\n";

const TYPES: &str = "package repro\n\
    class Entry(val id: String)\n\
    class RootDto(val entries: List<Entry>)\n";

#[test]
fn member_property_constructor_lambda_takes_its_parameter_from_a_sibling_argument() {
    // The corpus shape: named arguments, the constructor stored in a member property, `ROOT` pinned
    // only by the `Wrapper<RootDto>` argument and `DOMAIN` only by the lambda's own result.
    const MAIN: &str = "package repro\n\
        class Repo {\n\
            private val store =\n\
                Store(\n\
                    wrapper = Wrapper<RootDto>(\"repo\"),\n\
                    empty = RootDto(listOf(Entry(\"a\"), Entry(\"b\"))),\n\
                    toDomain = { dto -> dto.entries },\n\
                    toRoot = { list -> RootDto(list) },\n\
                )\n\
            fun findById(id: String): Entry? = store.domain().find { it.id == id }\n\
            fun label(): String = store.label()\n\
        }\n\
        fun box(): String {\n\
            val repo = Repo()\n\
            if (repo.findById(\"b\")?.id != \"b\") return \"fail find\"\n\
            if (repo.findById(\"z\") != null) return \"fail miss\"\n\
            if (repo.label() != \"repo\") return \"fail label\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Store.kt", STORE), ("Types.kt", TYPES), ("Main.kt", MAIN)],
        "ctor lambda parameter from a sibling argument",
    );
}

#[test]
fn named_arguments_out_of_declaration_order_bind_across_files() {
    // The same mapping in the signature pre-pass, where the class is another file's declaration.
    const MAIN: &str = "package repro\n\
        class Repo {\n\
            private val store =\n\
                Store(\n\
                    toRoot = { list -> RootDto(list) },\n\
                    toDomain = { dto -> dto.entries },\n\
                    empty = RootDto(listOf(Entry(\"a\"))),\n\
                    wrapper = Wrapper<RootDto>(\"repo\"),\n\
                )\n\
            fun all(): List<Entry> = store.domain()\n\
            fun label(): String = store.label()\n\
        }\n\
        fun box(): String {\n\
            val repo = Repo()\n\
            if (repo.all().single().id != \"a\") return \"fail all\"\n\
            if (repo.label() != \"repo\") return \"fail label\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Store.kt", STORE), ("Types.kt", TYPES), ("Main.kt", MAIN)],
        "named ctor arguments out of declaration order, cross-file",
    );
}

#[test]
fn a_secondary_constructor_that_cannot_take_the_call_leaves_the_template_alone() {
    // The negative half of the guard above. A declared parameter COUNT is not an arity — a default
    // makes shorter calls reachable, a vararg longer ones — so the question is whether a
    // constructor could TAKE this call. Withdrawing on "declares a secondary constructor at all"
    // loses the inference for an ordinary convenience constructor.
    const LIB: &str = "package repro\n\
        class Cell<T>(val value: T, val flag: Boolean) {\n\
            constructor(v: T) : this(v, true)\n\
            fun get(): T = value\n\
        }\n";
    const MAIN: &str = "package repro\n\
        class Repo {\n\
            val two = Cell(\"tag\", false)\n\
            val one = Cell(\"solo\")\n\
        }\n\
        fun box(): String {\n\
            val repo = Repo()\n\
            if (repo.two.get().length != 3) return \"fail two\"\n\
            if (repo.one.get() != \"solo\") return \"fail one\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "secondary constructor of another arity",
    );
}

#[test]
fn a_same_arity_secondary_constructor_keeps_the_template_out_of_it() {
    // The template describes the PRIMARY constructor. `Cell(5, "tag")` resolves to the secondary
    // one, so binding `T` from the primary's `(T, Boolean)` gives `Cell<Int>` — a checkcast to
    // `Integer` on a `String`, emitted with no diagnostic.
    const LIB: &str = "package repro\n\
        class Cell<T>(val value: T, val flag: Boolean) {\n\
            constructor(n: Int, v: T) : this(v, n > 0)\n\
            fun get(): T = value\n\
        }\n";
    const MAIN: &str = "package repro\n\
        class Repo {\n\
            val cell = Cell(5, \"tag\")\n\
            fun label(): String = cell.get().toString()\n\
        }\n\
        fun box(): String = if (Repo().label() == \"tag\") \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "same-arity secondary constructor",
    );
}

#[test]
fn a_factory_function_that_cannot_take_the_call_leaves_the_template_alone() {
    // The negative half of the factory guard: a same-named function that cannot take THIS call does
    // not own it. `fun Cell(n: Int)` beside `class Cell<T>(value: T, flag: Boolean)` is ordinary.
    const LIB: &str = "package repro\n\
        class Cell<T>(val value: T, val flag: Boolean) {\n\
            fun get(): T = value\n\
        }\n\
        fun Cell(n: Int): String = \"n\" + n\n";
    const MAIN: &str = "package repro\n\
        class Repo { val cell = Cell(\"tag\", false) }\n\
        fun box(): String = if (Repo().cell.get().length == 3) \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "factory function of another arity",
    );
}

#[test]
fn a_same_named_factory_function_is_not_a_construction() {
    // A top-level `fun Cell(…)` (or a companion `invoke`) can own the call, so the constructor's
    // template must not decide the type arguments for it.
    const LIB: &str = "package repro\n\
        class Cell<T>(val value: T, val flag: Boolean) {\n\
            fun get(): T = value\n\
        }\n\
        fun Cell(n: Int, s: String): Cell<String> = Cell(s, n > 0)\n";
    const MAIN: &str = "package repro\n\
        class Reader {\n\
            val cell = Cell(5, \"tag\")\n\
            fun label(): String = cell.get().toString()\n\
        }\n\
        fun box(): String = if (Reader().label() == \"tag\") \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "same-named factory function",
    );
}

#[test]
fn a_companion_keeps_the_template_out_of_it() {
    // `C(…)` is `C.Companion.invoke(…)` whenever an `invoke` applies, and that `invoke` may be an
    // EXTENSION declared anywhere in the module — which the signature pass cannot resolve while
    // signatures are still being collected. A companion therefore claims the name.
    const LIB: &str = "package repro\n\
        class Boxed<T>(val value: T, val flag: Boolean) {\n\
            fun get(): T = value\n\
            companion object\n\
        }\n\
        operator fun Boxed.Companion.invoke(a: String, b: String): Boxed<Int> =\n\
            Boxed(a.length + b.length, true)\n";
    const MAIN: &str = "package repro\n\
        class Repo { val boxed = Boxed(\"xx\", \"yyy\") }\n\
        fun box(): String = if (Repo().boxed.get() == 5) \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "companion invoke extension",
    );
}

#[test]
fn a_plain_companion_object_does_not_cost_the_inference() {
    // `C(…)` is `C.Companion.invoke(…)` only where an `invoke` applies. Withdrawing on the mere
    // presence of a companion rejects the constants/`serializer()`/logger companions that most
    // Kotlin classes carry.
    const LIB: &str = "package repro\n\
        class Boxed<T>(val v: T) {\n\
            fun get(): T = v\n\
            companion object { const val TAG = \"boxed\" }\n\
        }\n";
    const MAIN: &str = "package repro\n\
        class Repo { val boxed = Boxed(\"hi\") }\n\
        fun box(): String = if (Repo().boxed.get().length == 2) \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "plain companion object",
    );
}

#[test]
fn a_vararg_secondary_constructor_does_not_veto_the_primary() {
    // A vararg candidate spans every arity, so "fits the arity" would let one convenience
    // constructor withdraw the template from every construction of the class. Kotlin resolves the
    // overlap in the primary's favour.
    const LIB: &str = "package repro\n\
        class Cell<T>(val v: T, val n: Int) {\n\
            constructor(vararg parts: String) : this(parts.first() as T, parts.size)\n\
            fun get(): T = v\n\
        }\n";
    const MAIN: &str = "package repro\n\
        class Repo { val cell = Cell(\"x\", 3) }\n\
        fun box(): String {\n\
            val repo = Repo()\n\
            if (repo.cell.get().length != 1) return \"fail get\"\n\
            if (repo.cell.n != 3) return \"fail n\"\n\
            return \"OK\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "vararg secondary constructor",
    );
}

#[test]
fn a_value_class_type_argument_still_infers() {
    // A value class carries correctly where the construction and the declaration are collected
    // together, and that is what the declaration this file can see keeps. (Through a module or
    // dependency declaration the argument reaches an erased constructor parameter unboxed — a
    // lowering gap this inference must not make reachable, so those stay unapplied.)
    const MAIN: &str = "package repro\n\
        @JvmInline value class Money(val cents: Int)\n\
        class Holder<T>(val v: T) { fun get(): T = v }\n\
        class Repo {\n\
            val money = Holder(Money(250))\n\
            val count = Holder(7u)\n\
        }\n\
        fun box(): String {\n\
            val repo = Repo()\n\
            val amount: Money = repo.money.get()\n\
            val count: UInt = repo.count.get()\n\
            return if (amount.cents == 250 && count == 7u) \"OK\" else \"fail\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "value-class type argument");
}

#[test]
fn a_nested_value_class_argument_is_withheld_too() {
    // The withhold is about the whole applied type, not its outermost spelling: `Box<List<Money>>`
    // reaches the same erased constructor parameter as `Box<Money>`. Committing it compiles and
    // then throws `ClassCastException` on the first read, so the construction stays unapplied and
    // the program is REJECTED rather than miscompiled.
    const LIB: &str = "package repro\n\
        @JvmInline value class Money(val amount: String)\n\
        class Box<T>(val v: T)\n";
    const MAIN: &str = "package repro\n\
        val boxed = Box(listOf(Money(\"1\"), Money(\"2\")))\n\
        fun box(): String = boxed.v[0].amount\n";
    let diagnostics = common::front_end_diagnostics_files_with_stdlib(&[LIB, MAIN]);
    assert!(
        !diagnostics.is_empty(),
        "a nested value-class type argument must not be committed: {diagnostics:?}"
    );
}

#[test]
fn an_aliased_factory_import_owns_the_call_it_names() {
    // `import other.makeCell as Cell` reads exactly like a construction. The import map is the
    // file's own text, so this holds whether or not the target package has been collected yet —
    // resolving the import's owner cannot answer during the signature pass.
    const LIB: &str = "package repro\n\
        class Cell<T>(val value: T, val flag: Boolean) { fun get(): T = value }\n";
    const OTHER: &str = "package other\n\
        import repro.Cell\n\
        fun makeCell(n: Int, s: String): Cell<String> = Cell(s, n > 0)\n";
    const MAIN: &str = "package repro\n\
        import other.makeCell as Cell\n\
        class Reader { val cell = Cell(5, \"tag\") }\n\
        fun box(): String = if (Reader().cell.get() == \"tag\") \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Other.kt", OTHER), ("Main.kt", MAIN)],
        "aliased factory import",
    );
}

#[test]
fn a_secondary_constructor_no_argument_fits_leaves_the_template_alone() {
    // Arity is not the question a claimant answers — types are. `Wrapper("hi")` cannot reach
    // `constructor(list: List<T>, index: Int = 0)`, which spans the same arity through its default.
    const LIB: &str = "package repro\n\
        class Wrapper<T>(val v: T) {\n\
            constructor(list: List<T>, index: Int = 0) : this(list[index])\n\
            fun get(): T = v\n\
        }\n";
    const MAIN: &str = "package repro\n\
        class Repo { val w = Wrapper(\"hi\") }\n\
        fun box(): String = if (Repo().w.get().length == 2) \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "secondary constructor no argument fits",
    );
}

#[test]
fn a_same_arity_factory_no_argument_fits_leaves_the_template_alone() {
    // Same question for a same-named function: `fun Cell(n: Int, m: Int)` shares the arity of
    // `Cell("tag", false)` and still cannot own it, so the constructor keeps the call.
    const LIB: &str = "package repro\n\
        class Cell<T>(val v: T, val flag: Boolean) { fun get(): T = v }\n\
        fun Cell(n: Int, m: Int): String = \"x\" + (n + m)\n";
    const MAIN: &str = "package repro\n\
        class Repo { val c = Cell(\"tag\", false) }\n\
        fun box(): String = if (Repo().c.get().length == 3) \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "same-arity factory no argument fits",
    );
}

#[test]
fn a_secondary_constructor_reached_by_subtyping_keeps_the_template_out_of_it() {
    // Which constructor a call could reach is ordinary SUBTYPING, not classifier identity: a
    // `Number` parameter takes an `Int` argument. Reading identity instead says the secondary
    // cannot own the call, binds `T` from the primary, and emits a checkcast against it.
    const LIB: &str = "package repro\n\
        class Cell<T>(val value: T, val flag: Boolean) {\n\
            constructor(n: Number, v: T) : this(v, false)\n\
            fun get(): T = value\n\
        }\n";
    const MAIN: &str = "package repro\n\
        val cell = Cell(1, \"hello\")\n\
        fun box(): String = cell.get().toString() + \"!\"\n";
    let output = common::compile_and_run_files_with_stdlib(&[("Lib.kt", LIB), ("Main.kt", MAIN)]);
    assert_eq!(
        output.as_deref(),
        Some("hello!"),
        "secondary reached by subtyping"
    );
}

#[test]
fn an_array_secondary_constructor_the_primary_cannot_displace_keeps_the_template_out_of_it() {
    // Kotlin's vararg preference decides only between APPLICABLE candidates. A module signature
    // cannot tell `vararg` from an ordinary array parameter, so the exception may only apply where
    // the primary can genuinely take the call — by type, not merely by arity.
    const LIB: &str = "package repro\n\
        class Cell<T>(val value: T, val flag: Boolean) {\n\
            constructor(marker: Int, values: Array<out T>) : this(values[0], true)\n\
        }\n";
    const MAIN: &str = "package repro\n\
        val cell = Cell(1, arrayOf(\"a\"))\n\
        fun box(): String = cell.value.toString() + \"!\" + cell.flag\n";
    let output = common::compile_and_run_files_with_stdlib(&[("Lib.kt", LIB), ("Main.kt", MAIN)]);
    assert_eq!(
        output.as_deref(),
        Some("a!true"),
        "array secondary the primary cannot take"
    );
}

#[test]
fn an_unrelated_invoke_does_not_cost_a_companion_its_inference() {
    // The companion rule is about an `invoke` that can apply to THIS companion. A plain top-level
    // `fun invoke()` — or an `invoke` extension on some other receiver — owns nothing here, and
    // withdrawing on the name alone disables the feature for every class that has a companion.
    const MAIN: &str = "package repro\n\
        class Boxed<T>(val v: T) {\n\
            fun get(): T = v\n\
            companion object { const val TAG = \"boxed\" }\n\
        }\n\
        fun invoke(): Int = 7\n\
        class Repo { val boxed = Boxed(\"hi\") }\n\
        fun box(): String = if (Repo().boxed.get().length == 2) \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "an unrelated invoke");
}
