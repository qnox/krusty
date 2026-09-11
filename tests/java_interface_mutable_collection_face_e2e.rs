//! A Java INTERFACE that extends a `java.util` collection is mutable in Kotlin, exactly as a class
//! that implements one is.
//!
//! `interface Bag extends Map<String, Object>` is `MutableMap<String!, Any!>` to kotlinc, so
//! `bag["k"] = v` resolves through `MutableMap.set` and the interface widens to `MutableMap`. krusty
//! withheld the mutable face from EVERY interface — a rule that is right only for the collection
//! interfaces themselves (`java/util/List` is the shared realization of both `List` and
//! `MutableList`, so tagging it mutable would let a read-only `List` satisfy a `MutableList`
//! parameter), and wrong for every other Java interface that happens to extend one.
//!
//! The read-only face was already published, which is what made this hard to see: the type resolved,
//! members were found, and only a MUTATING use failed.
use std::path::Path;

use super::common;

#[derive(Debug, PartialEq, Eq)]
struct ObservedDiagnostic {
    file: String,
    line: usize,
    column: usize,
    message: String,
}

fn located_error(line: &str) -> ObservedDiagnostic {
    let (location, message) = line
        .split_once(": error: ")
        .unwrap_or_else(|| panic!("unexpected compiler diagnostic: {line}"));
    let mut fields = location.rsplitn(3, ':');
    let column = fields
        .next()
        .expect("diagnostic column")
        .parse()
        .expect("numeric diagnostic column");
    let line = fields
        .next()
        .expect("diagnostic line")
        .parse()
        .expect("numeric diagnostic line");
    let path = fields.next().expect("diagnostic path");
    ObservedDiagnostic {
        file: Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 diagnostic filename")
            .to_string(),
        line,
        column,
        message: message.to_string(),
    }
}

fn assert_frontends_accept(filename: &str, src: &str, classpath: &[std::path::PathBuf]) {
    let result = common::compiler_diagnostics(&[(filename, src)], classpath);
    assert_eq!(
        (
            result.krusty_code,
            result.krusty_stderr.as_str(),
            result.reference_code,
            result.reference_stderr.as_str(),
        ),
        (0, "", 0, "")
    );
}

/// A Java interface extending `Map`, and a class implementing it, compiled by javac — the shape has
/// to come from real Java bytecode, since a Kotlin declaration states its own mutability.
fn library() -> Option<std::path::PathBuf> {
    let java = [
        (
            "Bag.java".into(),
            "package jb;\n\
             import java.util.Map;\n\
             public interface Bag extends Map<String, Object> {}\n"
                .into(),
        ),
        (
            "Bags.java".into(),
            "package jb;\n\
             import java.util.HashMap;\n\
             public final class Bags extends HashMap<String, Object> implements Bag {\n\
             \x20   public static Bag empty() { return new Bags(); }\n\
             }\n"
            .into(),
        ),
    ];
    common::javac_compile(&java, &[]).map(|(dir, _)| dir)
}

fn classpath() -> Option<Vec<std::path::PathBuf>> {
    Some(vec![library()?, common::stdlib_jar()])
}

#[test]
fn a_java_interface_extending_map_widens_to_a_mutable_map() {
    let Some(classpath) = classpath() else {
        eprintln!("skipping: javac unavailable");
        return;
    };
    let jdk = common::jdk_modules();
    const SRC: &str = "import jb.Bag\n\
        fun widen(b: Bag): MutableMap<String, Any?> = b\n";
    assert_frontends_accept("JavaInterfaceMutableWiden.kt", SRC, &classpath);
    assert_eq!(
        common::front_end_diagnostics(SRC, &classpath, Some(jdk.as_path())),
        Vec::<String>::new()
    );
}

#[test]
fn a_nullable_value_stores_into_a_java_interface_extending_map() {
    let Some(classpath) = classpath() else {
        return;
    };
    let jdk = common::jdk_modules();
    const SRC: &str = "import jb.Bag\n\
        fun store(b: Bag, v: String?) {\n\
        \x20 b[\"k\"] = v\n\
        }\n";
    assert_frontends_accept("JavaInterfaceMutableStore.kt", SRC, &classpath);
    assert_eq!(
        common::front_end_diagnostics(SRC, &classpath, Some(jdk.as_path())),
        Vec::<String>::new()
    );
}

/// A read-only `List` must NOT gain the mutable face along the way: the collection interfaces
/// themselves are the one case where withholding it is correct.
#[test]
fn a_read_only_list_still_refuses_a_mutable_list() {
    let Some(classpath) = classpath() else {
        return;
    };
    let jdk = common::jdk_modules();
    const SRC: &str = "fun widen(l: List<String>): MutableList<String> = l\n";
    let diagnostics = common::front_end_diagnostics(SRC, &classpath, Some(jdk.as_path()));
    assert_eq!(
        diagnostics,
        ["return type mismatch: expected 'MutableList<String>', actual 'List<String>'."]
    );
    let comparison = common::compiler_diagnostics(&[("ReadOnlyList.kt", SRC)], &classpath);
    assert_eq!((comparison.krusty_code, comparison.reference_code), (1, 1));
    assert_eq!(comparison.krusty_stdout, "");
    let krusty_lines = comparison.krusty_stderr.lines().collect::<Vec<_>>();
    let reference_lines = comparison.reference_stderr.lines().collect::<Vec<_>>();
    let expected = ObservedDiagnostic {
        file: "ReadOnlyList.kt".to_string(),
        line: 1,
        column: 51,
        message: "return type mismatch: expected 'MutableList<String>', actual 'List<String>'."
            .to_string(),
    };
    assert_eq!(krusty_lines.len(), 2);
    assert_eq!(located_error(krusty_lines[0]), expected);
    assert_eq!(krusty_lines[1], "krusty: 1 error(s)");
    assert_eq!(reference_lines.len(), 3);
    assert_eq!(located_error(reference_lines[0]), expected);
    assert_eq!(reference_lines[1], SRC.trim_end());
    assert_eq!(reference_lines[2], format!("{}^", " ".repeat(50)));
}

/// The store runs: the value reaches the underlying map through the interface-typed receiver.
#[test]
fn the_store_through_the_interface_round_trips() {
    let Some(classpath) = classpath() else {
        return;
    };
    let jdk = common::jdk_modules();
    const SRC: &str = "import jb.Bag\n\
        import jb.Bags\n\
        fun store(b: Bag, v: String?) {\n\
        \x20 b[\"k\"] = v\n\
        }\n\
        fun box(): String {\n\
        \x20 val bag = Bags.empty()\n\
        \x20 store(bag, \"OK\")\n\
        \x20 return bag[\"k\"] as? String ?: \"missing\"\n\
        }\n";
    assert_frontends_accept("JavaInterfaceMutableRuntime.kt", SRC, &classpath);
    let classes = common::compile_in_process(SRC, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(SRC, &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(
        common::run_box(&classes, "MainKt", &classpath).expect("box runner"),
        "OK"
    );
}
