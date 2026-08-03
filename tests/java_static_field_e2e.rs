use std::fs;
use std::process::Command;

use super::common;

#[test]
fn reads_java_static_fields() {
    let java_home = common::java_home();
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let root = std::env::temp_dir().join(format!("krusty_jsf_{}", std::process::id()));
    let cp = root.join("cp");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(cp.join("p")).unwrap();

    fs::write(
        cp.join("p/Flat.java"),
        "package p;\npublic class Flat {\n\
         public static final String NAME = \"flat\";\n\
         public static final int COUNT = 7;\n\
         public static String mut = \"mut\";\n\
         }\n",
    )
    .unwrap();
    fs::write(
        cp.join("p/Outer.java"),
        "package p;\npublic class Outer {\n\
         public static class Bus {\n\
         public static final String CONST = \"bus\";\n\
         }\n\
         }\n",
    )
    .unwrap();
    assert!(Command::new(&javac)
        .args(["-d", cp.to_str().unwrap()])
        .arg(cp.join("p/Flat.java"))
        .arg(cp.join("p/Outer.java"))
        .output()
        .unwrap()
        .status
        .success());

    let use_src = "import p.Flat\nimport p.Outer\nfun box(): String {\n\
         if (Flat.NAME != \"flat\") return \"f1\"\n\
         if (Flat.COUNT != 7) return \"f2\"\n\
         if (Flat.mut != \"changed\") return \"f3\"\n\
         if (Outer.Bus.CONST != \"bus\") return \"f4\"\n\
         return \"OK\"\n}\n";
    let kr = root.join("kr");
    assert!(
        common::compile_to_dir(use_src, "Use", std::slice::from_ref(&cp), Some(&jdk), &kr)
            .is_some(),
        "krusty failed on Java static field read"
    );
    fs::write(
        cp.join("p/Flat.java"),
        "package p;\npublic class Flat {\n\
         public static final String NAME = \"new-flat\";\n\
         public static final int COUNT = 9;\n\
         public static String mut = \"changed\";\n\
         }\n",
    )
    .unwrap();
    fs::write(
        cp.join("p/Outer.java"),
        "package p;\npublic class Outer {\n\
         public static class Bus {\n\
         public static final String CONST = \"new-bus\";\n\
         }\n\
         }\n",
    )
    .unwrap();
    assert!(Command::new(&javac)
        .args(["-d", cp.to_str().unwrap()])
        .arg(cp.join("p/Flat.java"))
        .arg(cp.join("p/Outer.java"))
        .output()
        .unwrap()
        .status
        .success());

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
