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
    if let Some(out) = common::run_box_against("typealias", LIB, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }
}
