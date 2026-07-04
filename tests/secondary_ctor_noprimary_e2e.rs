//! Classes with NO primary constructor — every constructor is a secondary `constructor(...)` that
//! delegates to `super(...)` (or implicitly) or to a sibling `this(...)`. kotlinc emits one `<init>`
//! per secondary ctor; a super-reaching ctor runs the field initializers + `init {}` blocks (in
//! source order) before its own body, a `this(...)`-delegating ctor runs only its body.

use super::common;

fn run_box(name: &str, src: &str) {
    if let Some(out) = common::compile_and_run_with_stdlib(src, "B") {
        assert_eq!(out, "OK", "{name}");
    }
}

#[test]
fn no_primary_single_secondary_implicit_super() {
    // A deferred `val` backing field assigned once in the sole secondary ctor (no primary ctor).
    run_box(
        "simple",
        "class A {\n  val value: String\n  constructor(o: String) { value = o }\n}\nfun box(): String = if (A(\"OK\").value == \"OK\") \"OK\" else \"fail\"\n",
    );
}

#[test]
fn no_primary_many_sinks_init_runs_per_ctor() {
    // Two super-reaching secondary ctors; each runs the `init {}` blocks + field initializers first.
    run_box(
        "manysinks",
        "var sideEffects: String = \"\"\nclass A {\n  var prop: String = \"\"\n  init { sideEffects += prop + \"first\" }\n  constructor(x: String) { prop = x; sideEffects += \"#third\" }\n  init { sideEffects += prop + \"#second\" }\n  constructor(x: Int) { prop += \"$x#int\"; sideEffects += \"#fourth\" }\n}\nfun box(): String {\n  val a1 = A(\"abc\")\n  if (a1.prop != \"abc\") return \"f1\"\n  if (sideEffects != \"first#second#third\") return \"f2: $sideEffects\"\n  sideEffects = \"\"\n  val a2 = A(123)\n  if (a2.prop != \"123#int\") return \"f3\"\n  if (sideEffects != \"first#second#fourth\") return \"f4: $sideEffects\"\n  return \"OK\"\n}\n",
    );
}

#[test]
fn no_primary_this_delegation_to_sibling() {
    // One secondary delegates to a sibling via `this(...)`; only the sibling runs init, the delegating
    // ctor adds its body on top.
    run_box(
        "thissibling",
        "var log: String = \"\"\nclass A {\n  var prop: String = \"\"\n  init { log += \"init;\" }\n  constructor(x: String) { prop = x; log += \"s1;\" }\n  constructor(n: Int): this(\"n$n\") { log += \"s2;\" }\n}\nfun box(): String {\n  val a = A(7)\n  if (a.prop != \"n7\") return \"f1: ${a.prop}\"\n  if (log != \"init;s1;s2;\") return \"f2: $log\"\n  return \"OK\"\n}\n",
    );
}

#[test]
fn no_primary_super_delegation_to_base() {
    // A secondary ctor explicitly delegating to a base-class constructor.
    run_box(
        "superbase",
        "open class B(val tag: String)\nclass A : B {\n  var prop: String = \"\"\n  init { prop += \"i\" }\n  constructor(x: String): super(x) { prop += x }\n}\nfun box(): String {\n  val a = A(\"O\")\n  if (a.tag != \"O\") return \"f1: ${a.tag}\"\n  if (a.prop != \"iO\") return \"f2: ${a.prop}\"\n  return \"OK\"\n}\n",
    );
}
