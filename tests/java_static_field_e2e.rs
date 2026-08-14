//! Java static fields read from Kotlin: `static final` constants must be INLINED at the krusty
//! compile (so a later recompile of the Java lib with NEW constant values does not change what the
//! Kotlin code observed), while a non-final `static` field is a real GETSTATIC read that sees the
//! recompiled value. The recompile step is the point of the test — it distinguishes inlining from
//! field reads at run time.

use std::fs;

use super::common;

#[test]
fn reads_java_static_fields() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();

    let flat_v1 = "package p;\npublic class Flat {\n\
         public static final String NAME = \"flat\";\n\
         public static final int COUNT = 7;\n\
         public static String mut = \"mut\";\n\
         }\n";
    let outer_v1 = "package p;\npublic class Outer {\n\
         public static class Bus {\n\
         public static final String CONST = \"bus\";\n\
         }\n\
         }\n";
    let (cp_v1, _) = common::javac_compile(
        &[
            ("Flat.java".to_string(), flat_v1.to_string()),
            ("Outer.java".to_string(), outer_v1.to_string()),
        ],
        &[],
    )
    .expect("javac (pooled JavaRunner) compiles the v1 Java lib");

    let use_src = "import p.Flat\nimport p.Outer\nfun box(): String {\n\
         if (Flat.NAME != \"flat\") return \"f1\"\n\
         if (Flat.COUNT != 7) return \"f2\"\n\
         if (Flat.mut != \"changed\") return \"f3\"\n\
         if (Outer.Bus.CONST != \"bus\") return \"f4\"\n\
         return \"OK\"\n}\n";
    let kr = common::scratch_dir().expect("scratch dir");
    common::compile_to_dir(
        use_src,
        "Use",
        std::slice::from_ref(&cp_v1),
        Some(jdk.as_path()),
        &kr,
    )
    .expect("krusty compiles the Java static field reads");

    // Recompile the Java lib with NEW constant values and a NEW non-final value: the constants were
    // inlined at the krusty compile (still observe v1), the non-final `mut` reads v2's "changed".
    let flat_v2 = "package p;\npublic class Flat {\n\
         public static final String NAME = \"new-flat\";\n\
         public static final int COUNT = 9;\n\
         public static String mut = \"changed\";\n\
         }\n";
    let outer_v2 = "package p;\npublic class Outer {\n\
         public static class Bus {\n\
         public static final String CONST = \"new-bus\";\n\
         }\n\
         }\n";
    let (cp_v2, _) = common::javac_compile(
        &[
            ("Flat.java".to_string(), flat_v2.to_string()),
            ("Outer.java".to_string(), outer_v2.to_string()),
        ],
        &[],
    )
    .expect("javac (pooled JavaRunner) compiles the v2 Java lib");

    let main =
        "public class M { public static void main(String[] a) { System.out.println(UseKt.box()); } }";
    let m_path = kr.join("M.java");
    fs::write(&m_path, main).unwrap();
    let kcp = format!(
        "{}:{}:{}",
        kr.to_str().unwrap(),
        cp_v2.to_str().unwrap(),
        stdlib.display()
    );
    let out = common::javac_run(m_path.to_str().unwrap(), &kcp, kr.to_str().unwrap(), "M")
        .expect("pooled JavaRunner runs the driver");
    assert_eq!(out.trim(), "OK", "run={out:?}");
}
