use std::fs;
use std::process::Command;

use super::common;

fn write_fixture(cp: &std::path::Path, javac: &str) {
    fs::create_dir_all(cp.join("lib")).unwrap();
    fs::write(
        cp.join("lib/Outer.java"),
        "package lib;\npublic class Outer {\n\
         public static class Bus {\n\
         public static String notify(String s) { return \"n:\" + s; }\n\
         public static int add(int a, int b) { return a + b; }\n\
         public static class Deep {\n\
         public static String id() { return \"deep\"; }\n\
         }\n\
         }\n\
         }\n",
    )
    .unwrap();
    assert!(Command::new(javac)
        .args(["-d", cp.to_str().unwrap()])
        .arg(cp.join("lib/Outer.java"))
        .output()
        .unwrap()
        .status
        .success());
}

fn run_box(use_src: &str, tag: &str) {
    let Some(java_home) = common::java_home() else {
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let (Some(jdk), Some(stdlib)) = (common::jdk_modules(), common::stdlib_jar()) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("krusty_jns_{tag}_{}", std::process::id()));
    let cp = root.join("cp");
    let _ = fs::remove_dir_all(&root);
    write_fixture(&cp, &javac);

    let kr = root.join("kr");
    assert!(
        common::compile_to_dir(use_src, "Use", std::slice::from_ref(&cp), Some(&jdk), &kr)
            .is_some(),
        "krusty failed on nested Java static call ({tag})"
    );

    let main = "public class M { public static void main(String[] a) { System.out.println(UseKt.box()); } }";
    let m_path = kr.join("M.java");
    fs::write(&m_path, main).unwrap();
    let kcp = format!(
        "{}:{}:{}",
        kr.to_str().unwrap(),
        cp.to_str().unwrap(),
        stdlib.display()
    );
    let out = common::javac_run(m_path.to_str().unwrap(), &kcp, kr.to_str().unwrap(), "M");
    assert_eq!(out.as_deref().map(str::trim), Some("OK"), "run={out:?}");
    let _ = fs::remove_dir_all(&root);
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
