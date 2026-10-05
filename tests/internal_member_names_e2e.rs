//! An `internal` instance member is public in bytecode and named `name$<module>`. A public
//! override keeps the Kotlin name and adds an `ACC_PUBLIC|ACC_BRIDGE|ACC_SYNTHETIC` method of the
//! mangled name. Top-level functions, constructors, private members, and `@PublishedApi` members
//! stay unmangled. The suffix follows value-class mangling (`m-HASH$module`, `m-impl$module`).
//! A companion `@JvmStatic` member is mangled on the companion; the enclosing class's static
//! forwarder is a separate shape and is not part of this comparison.

use std::collections::BTreeSet;

use super::common;
use krusty::jvm::classreader::parse_class;

const SOURCE: &str = "\
package demo

@JvmInline
value class Tag(val raw: Int)

open class Base {
    internal fun hidden(): Int = 1
    internal open fun openHidden(): Int = 2
    internal var v: Int = 3
    private fun p(): Int = 4
    @PublishedApi
    internal open fun published(): Int = 5
    @PublishedApi
    internal val publishedVal: Int = 6
    internal fun tagged(t: Tag): Int = t.raw
    internal open fun openTagged(t: Tag): Int = t.raw
    internal fun d(x: Int = 1): Int = x
    internal open var openV: Int = 9
        get() = field
        set(value) { field = value }
}

class Child : Base() {
    public override fun openHidden(): Int = 20
    public override fun openTagged(t: Tag): Int = t.raw + 1
    public override var openV: Int = 90
        get() = field
        set(value) { field = value }
}

open class Stay {
    internal open fun stay(): Int = 1
}

class Same : Stay() {
    internal override fun stay(): Int = 2
}

internal fun top(): Int = 7

object O {
    internal fun m(): Int = 8
}

class A {
    companion object {
        @JvmStatic
        internal fun s(): Int = 1
        internal fun m(): Int = 2
    }
}

@JvmInline
value class V(val raw: Int) {
    internal fun m(): Int = raw
    internal val p: Int get() = raw
}
";

/// Methods that are not the member-name contract: `Any`, constructors, and the value-class
/// wrappers kotlinc synthesizes beside a user member.
fn user_methods(bytes: &[u8]) -> BTreeSet<String> {
    let class = parse_class(bytes).expect("class parses");
    class
        .methods
        .iter()
        .filter(|method| {
            !matches!(
                method.name.as_str(),
                "<init>"
                    | "<clinit>"
                    | "equals"
                    | "hashCode"
                    | "toString"
                    | "box-impl"
                    | "unbox-impl"
                    | "constructor-impl"
                    | "equals-impl"
                    | "equals-impl0"
                    | "hashCode-impl"
                    | "toString-impl"
            )
        })
        .map(|method| {
            format!(
                "{:#06x} {}{}",
                method.access, method.name, method.descriptor
            )
        })
        .collect()
}

fn user_fields(bytes: &[u8]) -> BTreeSet<String> {
    let class = parse_class(bytes).expect("class parses");
    class
        .fields
        .iter()
        .map(|field| format!("{:#06x} {}{}", field.access, field.name, field.descriptor))
        .collect()
}

fn krusty_classes(module_name: &str) -> Vec<(String, Vec<u8>)> {
    let classpath = [common::stdlib_jar()];
    common::compile_in_process_metadata_cp_module(SOURCE, "Members", &classpath, module_name)
        .unwrap_or_else(|| {
            let diagnostics = common::front_end_diagnostics(SOURCE, &classpath, None);
            panic!("krusty declined the source under {module_name}: {diagnostics:?}")
        })
}

fn kotlinc_classes(module_name: &str) -> Option<Vec<(String, Vec<u8>)>> {
    common::java_home();
    let dir = common::scratch_dir()?;
    let out = dir.join("out");
    std::fs::create_dir_all(&out).ok()?;
    let source = dir.join("Members.kt");
    std::fs::write(&source, SOURCE).ok()?;
    let args = vec![
        "-module-name".to_string(),
        module_name.to_string(),
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args)?;
    assert_eq!(code, 0, "kotlinc failed under {module_name}: {stderr}");
    let mut classes = Vec::new();
    collect_classes(&out, &out, &mut classes);
    let _ = std::fs::remove_dir_all(&dir);
    Some(classes)
}

fn collect_classes(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<(String, Vec<u8>)>,
) {
    let entries = std::fs::read_dir(dir).expect("read kotlinc output");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_classes(root, &path, out);
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "class") {
            let internal = path
                .strip_prefix(root)
                .expect("class stays under the output root")
                .to_string_lossy()
                .trim_end_matches(".class")
                .replace('\\', "/");
            out.push((internal, std::fs::read(&path).expect("read class")));
        }
    }
}

fn class_bytes<'a>(classes: &'a [(String, Vec<u8>)], internal: &str) -> &'a [u8] {
    classes
        .iter()
        .find(|(name, _)| name == internal)
        .map(|(_, bytes)| bytes.as_slice())
        .unwrap_or_else(|| panic!("{internal} was not emitted"))
}

fn assert_matches(module_name: &str, internals: &[&str]) {
    let Some(reference) = kotlinc_classes(module_name) else {
        eprintln!("skip ({module_name}: provisioned kotlinc unavailable)");
        return;
    };
    let krusty = krusty_classes(module_name);
    let mut mismatches = Vec::new();
    for internal in internals {
        let reference_bytes = class_bytes(&reference, internal);
        let krusty_bytes = class_bytes(&krusty, internal);
        let krusty_methods = user_methods(krusty_bytes);
        let reference_methods = user_methods(reference_bytes);
        if krusty_methods != reference_methods {
            mismatches.push(format!(
                "{internal} methods under module {module_name}\n krusty: {krusty_methods:?}\n kotlinc: {reference_methods:?}"
            ));
        }
        let krusty_fields = user_fields(krusty_bytes);
        let reference_fields = user_fields(reference_bytes);
        if krusty_fields != reference_fields {
            mismatches.push(format!(
                "{internal} fields under module {module_name}\n krusty: {krusty_fields:?}\n kotlinc: {reference_fields:?}"
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

#[test]
fn internal_instance_members_match_kotlinc_under_an_explicit_module() {
    assert_matches(
        "lib1",
        &[
            "demo/Base",
            "demo/Child",
            "demo/Stay",
            "demo/Same",
            "demo/MembersKt",
            "demo/O",
            "demo/A$Companion",
            "demo/V",
        ],
    );
}

#[test]
fn the_default_module_suffix_is_main() {
    assert_matches("main", &["demo/Base", "demo/Child", "demo/MembersKt"]);
}
