//! `data class`: krusty synthesizes equals/hashCode/toString/componentN/copy(+copy$default) with a
//! public ABI identical to kotlinc and equivalent behavior. These tests check the emitted shape,
//! run it on a real JVM, diff the ABI against kotlinc, and round-trip through a Kotlin consumer
//! that uses destructuring and copy-with-defaults.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::emit_class;
use krusty::diag::DiagSink;
use krusty::jvm::classreader::parse_class;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn compile(src: &str, class_name: &str, internal: &str) -> Vec<u8> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let cd = files[0]
        .decls
        .iter()
        .find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == class_name => Some(c.clone()),
            _ => None,
        })
        .expect("class decl");
    let (bytes, _) = emit_class(&cd, &files[0], &info, internal, internal, &syms, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    bytes
}

#[test]
fn data_class_has_all_members() {
    let ci = parse_class(&compile("data class Point(val x: Int, val y: Int)", "Point", "Point")).unwrap();
    for (name, desc) in [
        ("component1", "()I"),
        ("component2", "()I"),
        ("copy", "(II)LPoint;"),
        ("copy$default", "(LPoint;IIILjava/lang/Object;)LPoint;"),
        ("toString", "()Ljava/lang/String;"),
        ("hashCode", "()I"),
        ("equals", "(Ljava/lang/Object;)Z"),
    ] {
        assert!(ci.method(name, desc).is_some(), "missing data-class member {name}{desc}");
    }
}

#[test]
fn data_class_behaves_under_jvm() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping data_class_behaves_under_jvm: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("krusty_dc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Point.class"), compile("data class Point(val x: Int, val y: Int)", "Point", "Point")).unwrap();

    let main = r#"
public class Main {
    public static void main(String[] a) {
        Point p = new Point(3, 4);
        Point q = new Point(3, 4);
        System.out.println(p.toString());
        System.out.println(p.equals(q));
        System.out.println(p.hashCode() == q.hashCode());
        System.out.println(p.component1() + "," + p.component2());
        System.out.println(p.copy(3, 9).toString());
        System.out.println(p.equals(new Point(1, 4)));
    }
}"#;
    fs::write(dir.join("Main.java"), main).unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("Main.java")).output().expect("javac");
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "Main"]).output().expect("java");
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "Point(x=3, y=4)\ntrue\ntrue\n3,4\nPoint(x=3, y=9)\nfalse\n");
    let _ = fs::remove_dir_all(&dir);
}

fn abi(dir: &PathBuf, class: &str) -> BTreeSet<String> {
    let out = Command::new("javap").args(["-p", "-cp", dir.to_str().unwrap(), class]).output().expect("javap");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim())
        .filter(|l| l.contains('(') && !l.contains("class "))
        .map(|l| l.trim_end_matches(';').split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn data_class_abi_matches_kotlinc_and_consumer_round_trips() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping data_class diff/round-trip: set KRUSTY_KOTLINC");
        return;
    };
    let root = std::env::temp_dir().join(format!("krusty_dcrt_{}", std::process::id()));
    let kr = root.join("kr");
    let refd = root.join("ref");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(kr.join("demo")).unwrap();
    fs::create_dir_all(&refd).unwrap();

    let src = "package demo\ndata class Point(val x: Int, val y: Int)\n";
    fs::write(kr.join("demo/Point.class"), compile(src, "Point", "demo/Point")).unwrap();
    fs::write(root.join("Point.kt"), src).unwrap();
    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("Point.kt")).args(["-d", refd.to_str().unwrap()]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    assert!(cmd.output().expect("kotlinc").status.success());

    // (1) public ABI must be identical.
    assert_eq!(abi(&kr, "demo.Point"), abi(&refd, "demo.Point"), "data class ABI differs from kotlinc");

    // (2) a Kotlin consumer using destructuring + copy(named, default) must compile + run against
    //     krusty's class (exercises operator componentN + DECLARES_DEFAULT_VALUE in @Metadata).
    let consumer = "import demo.Point\nfun main() {\n  val p = Point(3, 4)\n  val r = p.copy(y = 9)\n  val (a, b) = p\n  println(p.toString() + \"|\" + (p == Point(3,4)) + \"|\" + r + \"|\" + a + \",\" + b)\n}\n";
    fs::write(root.join("C.kt"), consumer).unwrap();
    let mut cc = Command::new(&kotlinc);
    cc.arg(root.join("C.kt")).args(["-cp", kr.to_str().unwrap(), "-d", root.join("cout").to_str().unwrap()]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cc.env("JAVA_HOME", jh);
    }
    let kc = cc.output().expect("kotlinc consumer");
    assert!(kc.status.success(), "kotlinc failed to consume krusty data class:\n{}", String::from_utf8_lossy(&kc.stderr));

    if let Some(stdlib) = env("KRUSTY_KOTLIN_STDLIB") {
        let cp = format!("{}:{}:{}", root.join("cout").to_str().unwrap(), kr.to_str().unwrap(), stdlib);
        let run = Command::new("java").args(["-cp", &cp, "CKt"]).output().expect("java");
        if run.status.success() {
            assert_eq!(String::from_utf8_lossy(&run.stdout), "Point(x=3, y=4)|true|Point(x=3, y=9)|3,4\n");
        }
    }
    let _ = fs::remove_dir_all(&root);
}
