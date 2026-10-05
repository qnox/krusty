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
    internal fun `looks$main`(): Int = 10
    internal fun `looks$lib1`(): Int = 11
    fun `public$main`(): Int = 12
    fun `public$lib1`(): Int = 13
    internal val `p$main`: Int get() = 14
    internal val `p$lib1`: Int get() = 15
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

open class Over {
    internal fun f(x: Int): Int = x
    fun f(x: String): Int = x.length
    internal open fun g(x: Int): Int = x
    open fun g(x: String): Int = x.length
}

class OverChild : Over() {
    public override fun g(x: Int): Int = x + 1
    public override fun g(x: String): Int = x.length + 1
}

fun box(): String {
    val base = Over()
    val child = OverChild()
    val direct = base.f(1) + base.f(\"ab\") + base.g(2) + base.g(\"cd\")
    val onChild = child.f(1) + child.f(\"xy\") + child.g(3) + child.g(\"z\")
    val asBase: Over = child
    val through = asBase.f(4) + asBase.f(\"abcd\") + asBase.g(5) + asBase.g(\"abcde\")
    val names = Base().run {
        `looks$main`() + `looks$lib1`() + `public$main`() + `public$lib1`() + `p$main` + `p$lib1`
    }
    return if (direct == 7 && onChild == 9 && through == 20 && names == 75) \"OK\" else \"direct=$direct child=$onChild through=$through names=$names\"
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

fn krusty_source_classes(source: &str, stem: &str, module_name: &str) -> Vec<(String, Vec<u8>)> {
    let classpath = [common::stdlib_jar()];
    common::compile_in_process_metadata_cp_module(source, stem, &classpath, module_name)
        .unwrap_or_else(|| {
            let diagnostics = common::front_end_diagnostics(source, &classpath, None);
            panic!("krusty declined {stem} under {module_name}: {diagnostics:?}")
        })
}

fn kotlinc_source_classes(
    source: &str,
    file_name: &str,
    module_name: &str,
) -> Option<Vec<(String, Vec<u8>)>> {
    common::java_home();
    let dir = common::scratch_dir()?;
    let out = dir.join("out");
    std::fs::create_dir_all(&out).ok()?;
    let source_path = dir.join(file_name);
    std::fs::write(&source_path, source).ok()?;
    let args = vec![
        "-module-name".to_string(),
        module_name.to_string(),
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
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
    assert_source_matches(SOURCE, "Members", "Members.kt", module_name, internals);
}

fn run_box(source: &str, stem: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    common::compile_and_run_box(source, stem, &[common::stdlib_jar()], Some(jdk.as_path()))
}

fn assert_source_matches(
    source: &str,
    stem: &str,
    file_name: &str,
    module_name: &str,
    internals: &[&str],
) {
    let Some(reference) = kotlinc_source_classes(source, file_name, module_name) else {
        eprintln!("skip ({module_name}: provisioned kotlinc unavailable)");
        return;
    };
    let krusty = krusty_source_classes(source, stem, module_name);
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
            "demo/Over",
            "demo/OverChild",
        ],
    );
}

#[test]
fn internal_and_public_overloads_keep_distinct_jvm_names() {
    let result = common::compile_and_run_with_stdlib(SOURCE, "Members");
    assert_eq!(result.as_deref(), Some("OK"));
}

const DEFAULT_OVERLOADS: &str = "\
package demo

@JvmInline
value class W(val raw: Int) {
    internal fun f(x: Int = 1, y: Int = 2): Int = raw + x + y
    fun f(s: String = \"a\", t: String = \"b\"): Int = s.length + t.length
}

fun box(): String {
    val w = W(10)
    val internalDefault = w.f(x = 4)
    val publicDefault = w.f(s = \"abcd\")
    return if (internalDefault == 16 && publicDefault == 5) \"OK\" else \"i=$internalDefault p=$publicDefault\"
}
";

/// An internal value-class member and a public overload share the Kotlin name and both have
/// defaults. The omitted call must follow the internal stub's descriptor (`f-impl$<module>$default`)
/// and leave the public stub on `f-impl$default`.
#[test]
fn internal_default_stub_keeps_its_descriptor_beside_a_public_overload() {
    assert_source_matches(
        DEFAULT_OVERLOADS,
        "Defaults",
        "Defaults.kt",
        "main",
        &["demo/W"],
    );
    let result = run_box(DEFAULT_OVERLOADS, "Defaults");
    assert_eq!(result.as_deref(), Some("OK"));
}

const SIBLING_HOST: &str = "\
package demo

open class Host {
    internal fun f(x: Int = 1, y: Int = 2): Int = x + y
    fun f(s: String = \"a\", t: String = \"b\"): Int = s.length + t.length
}
";

const SIBLING_USE: &str = "\
package demo

fun box(): String {
    val host = Host()
    val internalDefault = host.f(x = 4)
    val publicDefault = host.f(s = \"abcd\")
    return if (internalDefault == 6 && publicDefault == 5) \"OK\" else \"i=$internalDefault p=$publicDefault\"
}
";

/// A default call in another file of the module names the selected declaration. The internal
/// stub gains `$<module>` before `$default`; the public overload's stub does not.
#[test]
fn sibling_default_call_uses_the_selected_declaration() {
    let result = common::compile_and_run_files_with_stdlib(&[
        ("Host.kt", SIBLING_HOST),
        ("Use.kt", SIBLING_USE),
    ]);
    assert_eq!(result.as_deref(), Some("OK"));
}

const INTERNAL_VALUE_OVERRIDE: &str = "\
package demo

@JvmInline
value class Id(val raw: Int)

abstract class Holder<T> {
    internal abstract fun build(): T
}

class IdHolder : Holder<Id>() {
    override fun build(): Id = Id(7)
}

@JvmInline
value class Z(internal val x: Int)

fun box(): String {
    val held: Holder<Id> = IdHolder()
    val built = held.build().raw
    val read = Z::x.get(Z(42))
    return if (built == 7 && read == 42) \"OK\" else \"built=$built read=$read\"
}
";

/// An internal override whose result is a value class keeps the module suffix on the mangled
/// member (`build-<hash>$main`). The erased bridge calls that member. A property reference to an
/// internal value-class constructor property calls `getX$main`.
#[test]
fn internal_value_class_override_and_property_reference_use_the_suffixed_name() {
    assert_source_matches(
        INTERNAL_VALUE_OVERRIDE,
        "InternalValue",
        "InternalValue.kt",
        "main",
        &["demo/Holder", "demo/IdHolder"],
    );
    let result = run_box(INTERNAL_VALUE_OVERRIDE, "InternalValue");
    assert_eq!(result.as_deref(), Some("OK"));
}

#[test]
fn the_default_module_suffix_is_main() {
    assert_matches(
        "main",
        &[
            "demo/Base",
            "demo/Child",
            "demo/MembersKt",
            "demo/Over",
            "demo/OverChild",
        ],
    );
}
