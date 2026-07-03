//! Repros for classpath type-reference / constructor / operator resolution:
//!  b1 zero-arg / all-default classpath constructor  (`Id()` for `value class Id(val v: String = "x")`)
//!  b2 fully-qualified dotted type ref inline         (`fun f(x: lib.Thing?)`, no import)
//!  b3 nested type in type position                   (`fun f(b: Wrap.Box)`)
//!  b5 comparison operator on a `Comparable` classpath type (`a < b` where `a,b: Money`)
mod common;

#[test]
fn classpath_type_ref_and_operator_resolution() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: no JDK modules");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let Some(libout) = common::compile_lib(
        "ctr",
        "package lib\n\
         @JvmInline value class Id(val v: String = \"x\")\n\
         class Cfg(val n: Int = 7, val s: String = \"d\")\n\
         class Thing(val z: Int)\n\
         class Wrap { class Box(val n: Int) }\n\
         class Money(val cents: Int) : Comparable<Money> {\n\
         \x20 override fun compareTo(other: Money): Int = cents - other.cents\n\
         }\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Id\n\
        import lib.Cfg\n\
        import lib.Wrap\n\
        import lib.Money\n\
        fun thingZ(x: lib.Thing?): Int = x?.z ?: -1\n\
        fun boxN(b: Wrap.Box): Int = b.n\n\
        fun box(): String {\n\
        \x20 if (Id().v != \"x\") return \"fail b1a: ${Id().v}\"\n\
        \x20 val c = Cfg()\n\
        \x20 if (c.n != 7 || c.s != \"d\") return \"fail b1b\"\n\
        \x20 if (thingZ(lib.Thing(5)) != 5) return \"fail b2\"\n\
        \x20 if (thingZ(null) != -1) return \"fail b2n\"\n\
        \x20 if (boxN(Wrap.Box(9)) != 9) return \"fail b3\"\n\
        \x20 val a = Money(3)\n\
        \x20 val d = Money(7)\n\
        \x20 if (!(a < d)) return \"fail b5-lt\"\n\
        \x20 if (a >= d) return \"fail b5-ge\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile classpath type-ref/operator resolution");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
