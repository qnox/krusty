//! A classpath `typealias` (`typealias Alias = Real`) imported and used unqualified — as a constructor
//! (`Alias(5)`) and in a type position (`fun f(x: Alias)`). Both were `unresolved` before: a top-level
//! type alias lands in its FILE FACADE's `@Metadata` (`LibKt`), not only the stdlib's dedicated
//! `*TypeAliasesKt` files, so the classpath scan must read `Package.typeAlias` from every `*Kt` facade.
//! The library is built by the real kotlinc via the shared `common::run_box_against` harness.
use super::common;

const LIB: &str = "package lib\n\
     class Real(val n: Int) { fun get(): Int = n }\n\
     data class Box(val v: String)\n\
     typealias Alias = Real\n\
     typealias BoxAlias = Box\n\
     typealias Chain = Alias\n";

#[test]
fn classpath_typealias_ctor_and_type_position() {
    let main = "import lib.Alias\n\
        import lib.BoxAlias\n\
        import lib.Chain\n\
        fun useParam(x: Alias): Int = x.get()\n\
        fun makeRet(): Alias = Alias(9)\n\
        fun box(): String {\n\
        \x20 val a = Alias(5)\n\
        \x20 if (a.get() != 5) return \"fail ctor: ${a.get()}\"\n\
        \x20 if (useParam(Alias(7)) != 7) return \"fail param\"\n\
        \x20 if (makeRet().get() != 9) return \"fail ret\"\n\
        \x20 val b = BoxAlias(\"hi\")\n\
        \x20 if (b.v != \"hi\") return \"fail box-alias: ${b.v}\"\n\
        \x20 val c = Chain(3)\n\
        \x20 if (c.get() != 3) return \"fail alias-chain: ${c.get()}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("typealias", LIB, main);
}

#[test]
fn pass_two_applies_generic_function_typealias_from_dependency() {
    let library = "package dependency\n\
        typealias Transform<T> = (T) -> String\n\
        class Consumer<T>(private val value: T) {\n\
        \x20 fun apply(transform: Transform<T>): String = transform(value)\n\
        }\n";
    let main = "import dependency.Consumer\n\
        import dependency.Transform\n\
        fun box(): String {\n\
        \x20 val transform: Transform<String> = { \"OK\" }\n\
        \x20 return Consumer(\"ignored\").apply(transform)\n\
        }\n";
    let Some(dependency) = common::compile_lib("generic_function_typealias", library) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert_eq!(
        common::compile_and_run_box(main, "Main", &[dependency, stdlib], Some(jdk.as_path()))
            .as_deref(),
        Some("OK"),
    );
}

#[test]
fn same_package_dependency_typealias_keeps_its_generic_function_template() {
    let library = r#"package fixture

data class Mark(val value: String)
class Input
typealias Transform<T> = (T) -> Mark

class Consumer<T>(private val value: T) {
    fun apply(transform: Transform<T>): Mark = transform(value)
}
"#;
    let main = r#"package fixture

fun box(): String {
    val transform: Transform<Input> = { Mark("OK") }
    return Consumer(Input()).apply(transform).value
}
"#;

    assert_eq!(
        common::expect_box_run_against("same_package_generic_function_typealias", library, main,)
            .as_deref(),
        Some("OK"),
    );
}

#[test]
fn classpath_typealias_visibility_is_enforced() {
    const VISIBILITY_LIB: &str = "package visibility\n\
        class Real\n\
        typealias PublicAlias = Real\n\
        internal typealias InternalAlias = Real\n\
        private typealias PrivateAlias = Real\n";
    let Some(libout) = common::compile_lib("typealias_visibility", VISIBILITY_LIB) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let cp = [libout.clone(), stdlib];
    let diagnostics = common::front_end_diagnostics(
        "import visibility.PublicAlias\n\
         import visibility.InternalAlias\n\
         import visibility.PrivateAlias\n\
         fun publicValue(): Any = PublicAlias()\n\
         fun internalValue(): Any = InternalAlias()\n\
         fun privateValue(): Any = PrivateAlias()\n",
        &cp,
        Some(jdk.as_path()),
    );

    assert!(diagnostics
        .iter()
        .any(|message| message.contains("InternalAlias")));
    assert!(diagnostics
        .iter()
        .any(|message| message.contains("PrivateAlias")));
    assert!(!diagnostics
        .iter()
        .any(|message| message.contains("PublicAlias")));

    if let Some(root) = libout.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn same_target_star_aliases_with_different_templates_are_ambiguous() {
    let Some(dependency) = common::compile_libs_ref(
        "same_target_star_alias_ambiguity",
        &[
            (
                "Pair.kt",
                "package fixture\nclass Pair<A, B>(val first: A, val second: B)\n",
            ),
            (
                "Left.kt",
                "package left\nimport fixture.Pair\ntypealias Choice<T> = Pair<T, T>\n",
            ),
            (
                "Right.kt",
                "package right\nimport fixture.Pair\nclass Marker\ntypealias Choice<T> = Pair<T, Marker>\n",
            ),
        ],
    ) else {
        return;
    };
    let source = "import left.*\n\
        import right.*\n\
        class Token\n\
        fun use(value: Choice<Token>): Token = Token()\n";
    let sources = [("Main.kt", source)];
    let result = common::compiler_diagnostics(&sources, &[dependency]);
    common::expect_identical_rejection(
        &result,
        "same-target star aliases with different templates",
    );
}
