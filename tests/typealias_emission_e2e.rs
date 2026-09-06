//! A krusty-compiled `typealias` must describe its expansion, not just its target classifier.
//!
//! kotlinc's `TypeAlias` record carries the alias's own type parameters and its expanded type WITH
//! arguments (`Box, S, A, PBox` for `typealias Box<S, A> = PBox<S, S, A, A>`). krusty emitted only
//! the target name, so a consumer reading a krusty-built library back could not place a use site's
//! arguments and produced the target's raw arity — the same divergence that consuming kotlinc's
//! aliases had, reproduced entirely inside krusty's own output.
//!
//! Every case reads a member whose type IS a substituted argument (`first: S`), so a raw target
//! hands back `Any` and fails the assignment rather than merely losing precision.
//! `expect_box_run_against` builds the dependency with KRUSTY (the self-consumption contract) and
//! cross-checks the same `main` against a kotlinc-built one.
use super::common;

const LIB: &str = "package lib\n\
    class PBox<S, T, A, B>(val tag: String, val first: S, val third: A)\n\
    class Marker(val value: String)\n\
    typealias Box<S, A> = PBox<S, S, A, A>\n\
    typealias Plain = PBox<String, String, Int, Int>\n\
    typealias Ignore<T> = Marker\n\
    typealias Handler<T> = (T) -> String\n\
    typealias Names<T> = List<T>\n\
    fun makeBox(): PBox<String, String, Int, Int> = PBox(\"made\", \"f\", 3)\n";

#[test]
fn krusty_built_alias_expands_to_the_targets_arguments() {
    // `Box<String, Int>` must mean `PBox<String, String, Int, Int>`: reading `first` as a String
    // and `third` as an Int only type-checks when the alias placed both of its arguments.
    const MAIN: &str = "import lib.Box\n\
        import lib.makeBox\n\
        fun box(): String {\n\
        \x20   val b: Box<String, Int> = makeBox()\n\
        \x20   val first: String = b.first\n\
        \x20   val third: Int = b.third\n\
        \x20   return if (first == \"f\" && third == 3) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        common::expect_box_run_against("tae1", LIB, MAIN).expect("toolchain"),
        "OK"
    );
}

#[test]
fn krusty_built_parameterless_alias_keeps_its_arguments() {
    // A parameterless alias supplies every argument itself; the raw target would erase them.
    const MAIN: &str = "import lib.Plain\n\
        import lib.makeBox\n\
        fun box(): String {\n\
        \x20   val b: Plain = makeBox()\n\
        \x20   val first: String = b.first\n\
        \x20   val third: Int = b.third\n\
        \x20   return if (first == \"f\" && third == 3) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        common::expect_box_run_against("tae2", LIB, MAIN).expect("toolchain"),
        "OK"
    );
}

#[test]
fn krusty_built_alias_chain_is_fully_expanded() {
    const CHAIN_LIB: &str = "package lib\n\
        class PBox<S, T, A, B>(val tag: String, val first: S)\n\
        typealias Box<S, A> = PBox<S, S, A, A>\n\
        typealias Chain<T> = Box<T, Int>\n\
        fun makeBox(): PBox<String, String, Int, Int> = PBox(\"made\", \"f\")\n";
    const MAIN: &str = "import lib.Chain\n\
        import lib.makeBox\n\
        fun box(): String {\n\
        \x20   val b: Chain<String> = makeBox()\n\
        \x20   val first: String = b.first\n\
        \x20   return if (b.tag == \"made\" && first == \"f\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        common::expect_box_run_against("tae3", CHAIN_LIB, MAIN).expect("toolchain"),
        "OK"
    );
}

#[test]
fn krusty_emits_formals_for_an_alias_over_a_non_generic_target() {
    const MAIN: &str = "import lib.Ignore\n\
        import lib.Marker\n\
        fun box(): String {\n\
        \x20   val marker: Ignore<Int> = Marker(\"OK\")\n\
        \x20   return marker.value\n\
        }\n";
    assert_eq!(
        common::expect_box_run_against("tae4", LIB, MAIN).expect("toolchain"),
        "OK"
    );
}

#[test]
fn krusty_emits_function_type_alias_expansions() {
    const MAIN: &str = "import lib.Handler\n\
        fun box(): String {\n\
        \x20   val handler: Handler<String> = { it }\n\
        \x20   return handler(\"OK\")\n\
        }\n";
    assert_eq!(
        common::expect_box_run_against("tae5", LIB, MAIN).expect("toolchain"),
        "OK"
    );
}

#[test]
fn krusty_emits_aliases_targeting_classpath_types() {
    const MAIN: &str = "import lib.Names\n\
        fun box(): String {\n\
        \x20   val names: Names<String> = listOf(\"OK\")\n\
        \x20   return names[0]\n\
        }\n";
    assert_eq!(
        common::expect_box_run_against("tae6", LIB, MAIN).expect("toolchain"),
        "OK"
    );
}

#[test]
fn redundant_alias_argument_on_a_member_property_stays_a_type_parameter() {
    const REDUNDANT_LIB: &str = "package lib\n\
        typealias Names<T> = List<String>\n\
        class Holder<T> { val names: Names<T>? = null }\n";
    const MAIN: &str = "import lib.Holder\n\
        fun box(): String = if (Holder<Int>().names == null) \"OK\" else \"fail\"\n";

    assert_eq!(
        common::expect_box_run_against("tae_redundant_member", REDUNDANT_LIB, MAIN)
            .expect("toolchain"),
        "OK"
    );
}

#[test]
fn dependency_alias_inference_preserves_a_local_nullable_property_parameter() {
    const ALIAS_LIB: &str = "package failure\n\
        typealias FailureOr<F> = Result<F>\n\
        class Result<out R>(val value: Any?)\n\
        class Failure<out E>(val error: E)\n\
        fun <U> failure(): FailureOr<U> = Result(Failure(Unit))\n\
        fun <T> success(value: T): Result<T> = Result(value)\n";
    const MAIN: &str = "import failure.*\n\
        class Single<S : Any>(val initialValue: S? = null)\n\
        fun <I, O> I.let(f: (I) -> O): O = f(this)\n\
        fun getLicense(key: String?): Single<FailureOr<String>> =\n\
            Single(key?.let { success(it) } ?: failure())\n\
        fun box(): String =\n\
            if (getLicense(null).initialValue?.value is Failure<*>) \"OK\" else \"fail\"\n";

    assert_eq!(
        common::expect_box_run_against("tae_nullable_property", ALIAS_LIB, MAIN)
            .expect("toolchain"),
        "OK"
    );
}
