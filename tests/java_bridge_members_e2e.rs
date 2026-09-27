//! A Java class's compiler-generated methods are ABI, not members, as kotlinc reads a class file:
//! a method flagged `ACC_BRIDGE` or `ACC_SYNTHETIC` takes no part in resolution. `Integer`'s
//! `compareTo(Object)` bridge made `Int < Char` applicable ahead of a user `Int.compareTo(Char)`
//! operator and failed with a ClassCastException at run time.

use std::path::{Path, PathBuf};

use super::common;

#[test]
fn a_user_operator_answers_what_no_primitive_member_takes() {
    let source = "\
        operator fun Int.compareTo(c: Char): Int = if (c == 'O') 1 else -1\n\
        fun above(x: Int, c: Char): Boolean = x > c\n\
        fun box(): String = if (above(42, 'O') && !above(42, 'K')) \"OK\" else \"fail\"\n\
        ";
    common::expect_box_same_as_kotlinc(source, "JavaBridgeMembers");
}

/// javac gives `StringSub` a `take(Object)` bridge (`ACC_BRIDGE | ACC_SYNTHETIC`) for the generic
/// override.
const GENERIC_BASE: &str = "package fixtures;\n\
    public class GenericBase<T> {\n\
    \x20 public String take(T value) { return \"base\"; }\n\
    }\n";
const STRING_SUB: &str = "package fixtures;\n\
    public class StringSub extends GenericBase<String> {\n\
    \x20 @Override public String take(String value) { return \"sub\"; }\n\
    }\n";

/// `pick` gets `ACC_SYNTHETIC` alone once compiled; `keep` stays an ordinary method.
const TOOLS: &str = "package fixtures;\n\
    public class Tools {\n\
    \x20 public String pick(Object value) { return \"member\"; }\n\
    \x20 public String keep(Object value) { return \"kept\"; }\n\
    }\n";

/// An `Int` argument does not fit the real `take(String)`, so the `Any` extension answers it; the
/// bridge taking `Object` would have been chosen as a member and thrown a ClassCastException.
#[test]
fn an_extension_answers_what_only_a_java_bridge_takes() {
    let classes = java_fixture(
        &[
            ("GenericBase.java", GENERIC_BASE),
            ("StringSub.java", STRING_SUB),
        ],
        None,
    );
    let user = "import fixtures.StringSub\n\
        fun StringSub.take(value: Any): String = \"ext\"\n\
        fun box(): String {\n\
        \x20 val sub = StringSub()\n\
        \x20 if (sub.take(1) != \"ext\") return \"bridge\"\n\
        \x20 if (sub.take(\"s\") != \"sub\") return \"member\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(runs_like_kotlinc(user, &classes), "OK");
}

/// Without the extension nothing takes an `Int`: kotlinc reports the real member's mismatch.
#[test]
fn a_java_bridge_does_not_accept_what_its_target_rejects() {
    let classes = java_fixture(
        &[
            ("GenericBase.java", GENERIC_BASE),
            ("StringSub.java", STRING_SUB),
        ],
        None,
    );
    let user = "import fixtures.StringSub\n\
        fun use(sub: StringSub): String = sub.take(1)\n";
    let result = common::compiler_diagnostics(
        &[("User.kt", user)],
        &[classes, common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&result, "bridge-only argument");
}

/// A method flagged `ACC_SYNTHETIC` without `ACC_BRIDGE` is not a member either, so the extension
/// answers `pick`, while the ordinary `keep` stays callable.
#[test]
fn an_extension_answers_what_only_a_java_synthetic_method_takes() {
    let classes = java_fixture(&[("Tools.java", TOOLS)], Some(("fixtures/Tools", "pick")));
    let user = "import fixtures.Tools\n\
        fun Tools.pick(value: Any): String = \"ext\"\n\
        fun box(): String {\n\
        \x20 val tools = Tools()\n\
        \x20 if (tools.pick(1) != \"ext\") return \"synthetic\"\n\
        \x20 if (tools.keep(1) != \"kept\") return \"ordinary\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(runs_like_kotlinc(user, &classes), "OK");
}

/// Without the extension the synthetic `pick` is unresolved, as kotlinc reports it.
#[test]
fn a_java_synthetic_method_is_not_a_member() {
    let classes = java_fixture(&[("Tools.java", TOOLS)], Some(("fixtures/Tools", "pick")));
    let user = "import fixtures.Tools\n\
        fun use(tools: Tools): String = tools.pick(1)\n";
    let result = common::compiler_diagnostics(
        &[("User.kt", user)],
        &[classes, common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&result, "synthetic-only member");
}

/// javac-compile `sources`, then set `ACC_SYNTHETIC` on `synthetic`'s `(class, method)` when given:
/// javac emits no public synthetic method from source.
fn java_fixture(sources: &[(&str, &str)], synthetic: Option<(&str, &str)>) -> PathBuf {
    let sources: Vec<(String, String)> = sources
        .iter()
        .map(|(name, source)| ((*name).to_string(), (*source).to_string()))
        .collect();
    let (classes, _) = common::javac_compile(&sources, &[]).expect("javac rejected the fixture");
    if let Some((class, method)) = synthetic {
        let path = classes.join(format!("{class}.class"));
        let mut bytes = std::fs::read(&path).expect("read the fixture class");
        mark_synthetic(&mut bytes, method);
        std::fs::write(&path, bytes).expect("write the fixture class");
    }
    classes
}

/// Set `ACC_SYNTHETIC` on the one method named `method` in a class file.
fn mark_synthetic(bytes: &mut [u8], method: &str) {
    const ACC_SYNTHETIC: u16 = 0x1000;
    let u2 = |bytes: &[u8], at: usize| u16::from_be_bytes([bytes[at], bytes[at + 1]]) as usize;
    let u4 = |bytes: &[u8], at: usize| {
        u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
    };
    let mut utf8 = std::collections::HashMap::new();
    let count = u2(bytes, 8);
    let mut at = 10;
    let mut index = 1;
    while index < count {
        let tag = bytes[at];
        at += 1;
        match tag {
            1 => {
                let len = u2(bytes, at);
                utf8.insert(index, bytes[at + 2..at + 2 + len].to_vec());
                at += 2 + len;
            }
            7 | 8 | 16 | 19 | 20 => at += 2,
            15 => at += 3,
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => at += 4,
            5 | 6 => {
                at += 8;
                index += 1;
            }
            other => panic!("unexpected constant tag {other}"),
        }
        index += 1;
    }
    at += 6;
    at += 2 + 2 * u2(bytes, at);
    let skip_members = |bytes: &[u8], mut at: usize, visit: &mut dyn FnMut(usize)| {
        let members = u2(bytes, at);
        at += 2;
        for _ in 0..members {
            visit(at);
            let attributes = u2(bytes, at + 6);
            at += 8;
            for _ in 0..attributes {
                at += 6 + u4(bytes, at + 2);
            }
        }
        at
    };
    at = skip_members(bytes, at, &mut |_| {});
    let mut found = Vec::new();
    skip_members(bytes, at, &mut |member| {
        if utf8.get(&u2(bytes, member + 2)).map(Vec::as_slice) == Some(method.as_bytes()) {
            found.push(member);
        }
    });
    assert_eq!(found.len(), 1, "one method named {method}");
    let flags = (u2(bytes, found[0]) as u16 | ACC_SYNTHETIC).to_be_bytes();
    bytes[found[0]..found[0] + 2].copy_from_slice(&flags);
}

/// Compile `user` against `classes` with both compilers and run each `box()`; both must agree.
fn runs_like_kotlinc(user: &str, classes: &Path) -> String {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let work = common::scratch_dir().expect("scratch filesystem");
    let source = work.join("Use.kt");
    std::fs::write(&source, user).expect("write the Kotlin fixture");
    let reference = work.join("reference");
    let (code, stderr) = common::kotlinc_compile(&[
        "-cp".to_string(),
        classes.to_string_lossy().into_owned(),
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference compiler unavailable");
    assert_eq!(
        (code, stderr.as_str()),
        (0, ""),
        "kotlinc rejected the fixture"
    );
    let krusty = work.join("krusty");
    common::compile_to_dir(
        user,
        "Use",
        &[classes.to_path_buf(), stdlib.clone()],
        Some(jdk.as_path()),
        &krusty,
    )
    .expect("krusty rejected the fixture");
    let driver = work.join("M.java");
    std::fs::write(
        &driver,
        "public class M { public static void main(String[] a) { System.out.println(UseKt.box()); } }",
    )
    .expect("write the driver");
    let run = |output: &Path| {
        let classpath = format!(
            "{}:{}:{}",
            output.display(),
            classes.display(),
            stdlib.display()
        );
        common::javac_run(
            &driver.to_string_lossy(),
            &classpath,
            &output.to_string_lossy(),
            "M",
        )
        .expect("pooled JavaRunner unavailable")
        .trim()
        .to_string()
    };
    let expected = run(&reference);
    assert_eq!(
        run(&krusty),
        expected,
        "krusty's box() differs from kotlinc's"
    );
    let _ = std::fs::remove_dir_all(work);
    expected
}
