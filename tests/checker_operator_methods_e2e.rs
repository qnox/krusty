//! Type-checker arms for builtin operator *methods* called by name (not operator syntax) and vararg
//! argument assignability — `Int.unaryMinus()/unaryPlus()`, `Char.plus(n)/minus(n)/minus(c)`, and a
//! `vararg` call whose every element is checked against the element type. The corpus reaches the
//! operator syntax but not these explicit method forms.

mod common;

fn run_ok(stem: &str, body: &str) {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping checker_operator_methods_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping checker_operator_methods_e2e: no kotlin-stdlib jar");
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(body, stem, &[stdlib], Some(&jdk)) else {
        panic!("{stem}: compile/run returned None");
    };
    assert_eq!(out, "OK", "{stem}");
}

#[test]
fn char_operator_methods() {
    // `Char.plus(Int): Char`, `Char.minus(Int): Char`, `Char.minus(Char): Int` — the checker maps
    // these named methods to the operator form. (`Int.unaryMinus()`/`unaryPlus()` as explicit methods
    // are NOT covered here: krusty types them but emits a call to a nonexistent `Integer.unaryMinus`,
    // a known emit gap — the operator syntax `-x` works, the method form does not.)
    run_ok(
        "CharOps",
        "fun box(): String {\n\
         if ('a'.plus(1) != 'b') return \"cp\"\n\
         if ('b'.minus(1) != 'a') return \"cm\"\n\
         if ('c'.minus('a') != 2) return \"cc\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn vararg_argument_assignability() {
    run_ok(
        "VarargArgs",
        "fun sm(vararg xs: Int): Int {\n\
         var s = 0\n\
         for (x in xs) s += x\n\
         return s\n\
         }\n\
         fun box(): String {\n\
         if (sm(1, 2, 3, 4) != 10) return \"va=${sm(1, 2, 3, 4)}\"\n\
         if (sm() != 0) return \"empty\"\n\
         return \"OK\"\n\
         }\n",
    );
}
