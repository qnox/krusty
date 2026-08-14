//! Nested Java static classes (`Outer.Bus.notify`, `Outer.Bus.Deep.id`) resolved through import
//! chains, direct nested imports, fully-qualified spellings, and a value shadowing the class name.
//! Runs through the pooled harness (`java_interop_box`) — no per-test javac/java spawns.

use super::common;

const OUTER: &str = "package lib;\npublic class Outer {\n\
     public static class Bus {\n\
     public static String notify(String s) { return \"n:\" + s; }\n\
     public static int add(int a, int b) { return a + b; }\n\
     public static class Deep {\n\
     public static String id() { return \"deep\"; }\n\
     }\n\
     }\n\
     }\n";

fn run_box(use_src: &str, tag: &str) {
    let out = common::java_interop_box(tag, &[("Outer.java", OUTER)], use_src);
    assert_eq!(out, "OK", "{tag}");
}

#[test]
fn nested_static_via_imported_outer_chain() {
    run_box(
        "import lib.Outer\nfun box(): String {\n\
         if (Outer.Bus.notify(\"x\") != \"n:x\") return \"f1\"\n\
         if (Outer.Bus.add(2, 3) != 5) return \"f2\"\n\
         if (Outer.Bus.Deep.id() != \"deep\") return \"f3\"\n\
         return \"OK\"\n}\n",
        "chain",
    );
}

#[test]
fn nested_static_via_direct_nested_import() {
    run_box(
        "import lib.Outer.Bus\nfun box(): String {\n\
         if (Bus.notify(\"x\") != \"n:x\") return \"f1\"\n\
         if (Bus.add(2, 3) != 5) return \"f2\"\n\
         return \"OK\"\n}\n",
        "import",
    );
}

#[test]
fn nested_static_via_fully_qualified_chain() {
    run_box(
        "fun box(): String {\n\
         if (lib.Outer.Bus.notify(\"x\") != \"n:x\") return \"f1\"\n\
         return \"OK\"\n}\n",
        "fq",
    );
}

#[test]
fn value_named_like_an_imported_outer_class_keeps_value_semantics() {
    run_box(
        "import lib.Outer\n\
         class LocalBus { fun notify(value: String): String = \"value:\" + value }\n\
         class Root(val Bus: LocalBus)\n\
         val Outer = Root(LocalBus())\n\
         fun box(): String = if (Outer.Bus.notify(\"x\") == \"value:x\") \"OK\" else \"fail\"\n",
        "shadow",
    );
}
