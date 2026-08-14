//! Java-interop breadth: constructing a classpath Java object (`Calc(10)`) and calling its
//! *instance* methods (`c.add(5)`, `c.tag()`), resolved via the `.class` reader → `invokespecial`
//! `<init>` + `invokevirtual`; and Java STATIC method resolution with overload selection by
//! argument type. Uses real javac-compiled classes through the pooled harness (in-process krusty
//! compile, persistent javac/JavaRunner JVMs — no per-test process spawns).

use super::common;

#[test]
fn constructs_and_calls_java_instance_methods() {
    let calc = "package util;\npublic class Calc {\n  private final int base;\n  public Calc(int base) { this.base = base; }\n  public int add(int n) { return base + n; }\n  public String tag() { return \"calc\"; }\n}\n";
    let use_src = "import util.Calc\nfun box(): String {\n  val c = Calc(10)\n  if (c.add(5) != 15) return \"f1\"\n  if (c.tag() != \"calc\") return \"f2\"\n  return \"OK\"\n}\n";
    let out = common::java_interop_box("java_instance", &[("Calc.java", calc)], use_src);
    assert_eq!(out, "OK");
}

/// Java (non-Kotlin) STATIC method resolution, including overload selection by argument type:
/// `Logf.make(String)` vs `Logf.make(Class)`, and `Logf.parse(String)` vs `Logf.parse(String, int)`.
/// krusty resolves the class-name receiver's static (from the `.class` reader → the type's static
/// list), picks the arity/type-appropriate overload, and emits `invokestatic`.
#[test]
fn calls_java_static_overloaded_methods() {
    let logf = "package lib;\npublic class Logf {\n\
         public static String make(String name) { return \"n:\" + name; }\n\
         public static String make(Class<?> c) { return \"c:\" + c.getSimpleName(); }\n\
         public static int parse(String s) { return Integer.parseInt(s); }\n\
         public static int parse(String s, int radix) { return Integer.parseInt(s, radix); }\n\
         }\n";
    let use_src = "import lib.Logf\nfun box(): String {\n\
         if (Logf.make(\"x\") != \"n:x\") return \"f1\"\n\
         if (Logf.parse(\"10\") != 10) return \"f2\"\n\
         if (Logf.parse(\"ff\", 16) != 255) return \"f3\"\n\
         return \"OK\"\n}\n";
    let out = common::java_interop_box("java_static_overloads", &[("Logf.java", logf)], use_src);
    assert_eq!(out, "OK");
}
