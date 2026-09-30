//! An `interface` may declare a `companion object` with `val` properties; they are emitted as static
//! fields on the interface and read as `C.X`. Runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn interface_companion_const_val() {
    const SRC: &str = "interface C {\n\
        \x20 companion object { const val FOO: String = \"OK\" }\n\
        }\n\
        fun box(): String = C.FOO\n";
    assert_eq!(run(SRC).expect("interface companion const"), "OK");
}

#[test]
fn interface_companion_non_const_val() {
    const SRC: &str = "interface C {\n\
        \x20 companion object { val FOO: String = \"O\" + \"K\" }\n\
        }\n\
        fun box(): String = C.FOO\n";
    assert_eq!(run(SRC).expect("interface companion non-const"), "OK");
}

#[test]
fn interface_companion_method() {
    const SRC: &str = "interface C {\n\
        \x20 companion object { fun make(): String = \"OK\" }\n\
        }\n\
        fun box(): String = C.make()\n";
    assert_eq!(run(SRC).expect("interface companion method"), "OK");
}

#[test]
fn interface_companion_method_and_prop() {
    const SRC: &str = "interface C {\n\
        \x20 companion object {\n\
        \x20   val P: String = \"O\" + \"K\"\n\
        \x20   fun make(): String = \"OK\"\n\
        \x20 }\n\
        }\n\
        fun box(): String = if (C.P == \"OK\" && C.make() == \"OK\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("interface companion method+prop"), "OK");
}

/// kotlinc 2.4.20: `Test.ok()` inside the companion initializer loads `$$INSTANCE`.
/// The interface `Companion` field is still null while that `<clinit>` runs.
#[test]
fn an_interface_companion_initializer_reads_its_own_instance() {
    const SRC: &str = "interface Test {\n\
        \x20   companion object {\n\
        \x20       fun ok() = \"OK\"\n\
        \x20       val x = run { Test.ok() }\n\
        \x20       fun test() = x\n\
        \x20   }\n\
        }\n\
        fun box() = Test.test()\n";
    assert_eq!(
        run(SRC).expect("companion initializer reads $$INSTANCE"),
        "OK"
    );
}

/// kotlinc 2.4.20: a companion method's `Test.ok()` also loads `$$INSTANCE`, not `Test.Companion`.
#[test]
fn an_interface_companion_method_reads_its_own_instance() {
    const SRC: &str = "interface Test {\n\
        \x20   companion object {\n\
        \x20       fun ok() = \"OK\"\n\
        \x20       fun method() = Test.ok()\n\
        \x20   }\n\
        }\n\
        fun box() = Test.method()\n";
    assert_eq!(run(SRC).expect("companion method reads $$INSTANCE"), "OK");
}

/// Field loads compared with kotlinc: companion self-reads use `$$INSTANCE`, an external caller
/// uses the interface `Companion` field, and `<clinit>` stores `$$INSTANCE` before any read.
#[test]
fn interface_companion_self_reads_match_kotlinc_fields() {
    const SRC: &str = "interface Test {\n\
        \x20   companion object {\n\
        \x20       fun ok() = \"OK\"\n\
        \x20       val x = run { Test.ok() }\n\
        \x20       fun test() = x\n\
        \x20       fun method() = Test.ok()\n\
        \x20   }\n\
        }\n\
        fun external(): Any = Test.Companion\n\
        fun box() = Test.test() + Test.method()\n";
    let reference = reference_field_ops(SRC);
    let ours = krusty_field_ops(SRC);
    for (class, method) in [
        ("Test$Companion", "method"),
        ("Test$Companion", "getX"),
        ("Test$Companion", "test"),
        ("Test", "<clinit>"),
        ("MainKt", "external"),
        ("MainKt", "box"),
    ] {
        assert_eq!(
            field_ops_of(&ours, class, method),
            field_ops_of(&reference, class, method),
            "{class}.{method} field loads diverged from kotlinc"
        );
    }
    assert_companion_clinit_stores_instance_before_any_read(&ours);
    assert_companion_clinit_stores_instance_before_any_read(&reference);
}

fn field_ops_of<'a>(
    ops: &'a [(String, String, Vec<String>)],
    class: &str,
    method: &str,
) -> &'a [String] {
    ops.iter()
        .find(|(owner, name, _)| owner == class && name == method)
        .map(|(_, _, operations)| operations.as_slice())
        .unwrap_or_else(|| panic!("{class}.{method} has no field operations"))
}

fn assert_companion_clinit_stores_instance_before_any_read(ops: &[(String, String, Vec<String>)]) {
    let companion_clinit = field_ops_of(ops, "Test$Companion", "<clinit>");
    let store = companion_clinit
        .iter()
        .position(|op| op == "putstatic $$INSTANCE")
        .expect("companion <clinit> stores $$INSTANCE");
    let first_get = companion_clinit
        .iter()
        .position(|op| op.starts_with("getstatic"))
        .expect("companion <clinit> reads the instance");
    assert!(
        store < first_get,
        "$$INSTANCE is stored before any field read: {companion_clinit:?}"
    );
    assert!(
        companion_clinit
            .iter()
            .any(|op| op == "getstatic $$INSTANCE"),
        "initializer self-read uses $$INSTANCE: {companion_clinit:?}"
    );
    assert!(
        companion_clinit
            .iter()
            .all(|op| !op.ends_with(" Companion")),
        "companion <clinit> must not touch the interface field: {companion_clinit:?}"
    );
}

fn reference_field_ops(src: &str) -> Vec<(String, String, Vec<String>)> {
    let work = common::scratch_dir().expect("scratch dir");
    let source = work.join("Main.kt");
    std::fs::write(&source, src).expect("write source");
    let out = work.join("ref");
    std::fs::create_dir_all(&out).expect("reference out");
    let (code, diagnostics) = common::kotlinc_compile(&[
        "-cp".to_string(),
        common::stdlib_jar().display().to_string(),
        "-d".to_string(),
        out.display().to_string(),
        source.display().to_string(),
    ])
    .expect("invoke kotlinc");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {diagnostics}");
    field_ops_in(&out)
}

fn krusty_field_ops(src: &str) -> Vec<(String, String, Vec<String>)> {
    let jdk = common::java_home();
    let modules = std::path::PathBuf::from(format!("{jdk}/lib/modules"));
    let classes = common::compile_in_process(
        src,
        "Main",
        &[common::stdlib_jar()],
        Some(modules.as_path()),
    )
    .expect("interface companion compiles");
    let out = common::scratch_dir().expect("scratch dir").join("krusty");
    std::fs::create_dir_all(&out).expect("krusty out");
    for (name, bytes) in &classes {
        let path = out.join(format!("{name}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("class parent");
        }
        std::fs::write(path, bytes).expect("write class");
    }
    field_ops_in(&out)
}

fn field_ops_in(root: &std::path::Path) -> Vec<(String, String, Vec<String>)> {
    ["Test$Companion", "Test", "MainKt"]
        .into_iter()
        .flat_map(|class| {
            let dump = common::javap(&["-c", "-p", "-cp", &root.to_string_lossy(), class])
                .unwrap_or_else(|| panic!("javap {class}"));
            method_field_ops(&dump)
                .into_iter()
                .map(move |(method, ops)| (class.to_string(), method, ops))
        })
        .filter(|(_, _, ops)| !ops.is_empty())
        .collect()
}

fn method_field_ops(dump: &str) -> Vec<(String, Vec<String>)> {
    let mut methods = Vec::new();
    let mut name = String::new();
    let mut ops = Vec::new();
    let mut in_method = false;
    for line in dump.lines() {
        if line.starts_with("  ") && !line.starts_with("   ") {
            if in_method {
                methods.push((std::mem::take(&mut name), std::mem::take(&mut ops)));
            }
            let header = line.trim().trim_end_matches(';');
            name = if header == "static {}" {
                "<clinit>".to_string()
            } else {
                header
                    .rsplit_once(' ')
                    .map(|(_, method)| method.trim_end_matches("()").to_string())
                    .unwrap_or_else(|| header.to_string())
            };
            in_method = true;
            continue;
        }
        if !in_method {
            continue;
        }
        let Some(comment) = line.split("// Field ").nth(1) else {
            continue;
        };
        let op = if line.contains("getstatic") {
            "getstatic"
        } else if line.contains("putstatic") {
            "putstatic"
        } else {
            continue;
        };
        let field = comment.split(':').next().unwrap_or(comment).trim();
        let field = field.rsplit('.').next().unwrap_or(field);
        ops.push(format!("{op} {field}"));
    }
    if in_method {
        methods.push((name, ops));
    }
    methods
}
