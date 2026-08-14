//! Classpath `protected` visibility: a Kotlin subclass of a javac-compiled Java base can call the
//! base's `protected` instance method (unqualified, through the inherited receiver) — kotlinc accepts
//! this, and the JVM allows the invoke because the caller is a subclass. krusty must surface the
//! `protected` member during the supertype member walk (not drop it as non-public) and resolve an
//! inherited classpath-superclass member of a user class at all.

use super::common;

#[test]
fn subclass_calls_protected_classpath_member() {
    // A Java base with a PROTECTED instance method — only reachable from a subclass.
    let base = "package lib;\npublic class Base {\n  protected int secret() { return 42; }\n}\n";
    // A Kotlin subclass calls the inherited protected member unqualified.
    let use_src = "import lib.Base\nclass Sub : Base() {\n  fun reveal(): Int = secret()\n}\n\
         fun box(): String {\n  if (Sub().reveal() != 42) return \"f1\"\n  return \"OK\"\n}\n";
    let out = common::java_interop_box("classpath_protected", &[("Base.java", base)], use_src);
    assert_eq!(out, "OK");
}
