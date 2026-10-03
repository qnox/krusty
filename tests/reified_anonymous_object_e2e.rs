//! An anonymous object created inside `inline fun <reified T>` is a separate class at each call.
//! The declaration class keeps the reified marker. The call-site class uses the type argument.

use super::common;
use std::path::Path;

fn collect_class_names(root: &Path, directory: &Path, names: &mut Vec<String>) {
    for entry in std::fs::read_dir(directory).expect("read compiler output") {
        let path = entry.expect("read compiler output entry").path();
        if path.is_dir() {
            collect_class_names(root, &path, names);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("class") {
            names.push(
                path.strip_prefix(root)
                    .expect("class below output root")
                    .with_extension("")
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
        }
    }
}

fn class_names(stem: &str, source: &str) -> (Vec<String>, Vec<String>) {
    let work = common::scratch_dir().expect("allocate anonymous-object fixture");
    let source_path = work.join(format!("{stem}.kt"));
    let krusty_output = work.join("krusty");
    let reference_output = work.join("reference");
    std::fs::write(&source_path, source).expect("write anonymous-object fixture");
    std::fs::create_dir_all(&krusty_output).expect("create krusty output");
    std::fs::create_dir_all(&reference_output).expect("create reference output");

    let krusty = std::process::Command::new(common::krusty_binary())
        .args(["-d", krusty_output.to_str().expect("UTF-8 output")])
        .arg("-no-reflect")
        .arg(&source_path)
        .output()
        .expect("run krusty");
    assert!(
        krusty.status.success(),
        "{stem}: krusty failed: {}",
        String::from_utf8_lossy(&krusty.stderr)
    );
    let reference_args = vec![
        "-d".to_string(),
        reference_output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        source_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) =
        common::kotlinc_compile(&reference_args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");

    let mut krusty_names = Vec::new();
    collect_class_names(&krusty_output, &krusty_output, &mut krusty_names);
    krusty_names.sort();
    let mut reference_names = Vec::new();
    collect_class_names(&reference_output, &reference_output, &mut reference_names);
    reference_names.sort();
    let _ = std::fs::remove_dir_all(work);
    (reference_names, krusty_names)
}

const INSTANCE: &str = "\
interface Face { fun bar(x: Any): Boolean }\n\
class Token\n\
class Other\n\
inline fun <reified T> b(): Face = object : Face {\n\
    override fun bar(x: Any): Boolean = x is T\n\
}\n\
fun box(): String {\n\
    val face = b<Token>()\n\
    if (!face.bar(Token()) || face.bar(Other())) return \"fail\"\n\
    return \"OK\"\n\
}\n";

#[test]
fn inline_anonymous_object_preserves_its_enclosing_reified_parameter() {
    let source = r#"
        interface I
        class C<T>

        private inline fun <reified T> C<T>.f() = object : I {
            val unused = T::class
        }

        fun box(): String {
            val first = C<String>().f()
            val second = C<String>().f()
            arrayOf(first, second)
            return "OK"
        }
    "#;

    let diagnostics = common::checker_diags_with_stdlib(source)
        .expect("the frontend test toolchain must be available");
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn a_reified_anonymous_instance_test_matches_kotlinc() {
    common::expect_box_same_as_kotlinc(INSTANCE, "ReifiedAnonymousInstance");
    let (reference, krusty) = class_names("ReifiedAnonymousInstance", INSTANCE);
    assert_eq!(krusty, reference);
    assert!(reference.iter().any(|name| name.contains("$$inlined$b$1")));
    assert!(reference
        .iter()
        .any(|name| name.ends_with("$b$1") && !name.contains("$$inlined$")));
}

const PROPERTIES: &str = "\
interface Cell<T> {\n\
    val item: T\n\
    var slot: T\n\
    fun shown(): String\n\
}\n\
class Token\n\
inline fun <reified T : Any> make(seed: T): Cell<T> = object : Cell<T> {\n\
    override val item: T\n\
        get() = if (T::class.simpleName != null) seed else seed\n\
    override var slot: T = seed\n\
        get() = field\n\
        set(value) {\n\
            val probe: Any = value\n\
            if (probe is T) field = value\n\
        }\n\
    val T.mark: Boolean\n\
        get() = T::class.simpleName != null\n\
    override fun shown(): String = if (item.mark) \"y\" else \"n\"\n\
}\n\
fun box(): String {\n\
    val cell = make(Token())\n\
    val first = cell.item\n\
    if (cell.shown() != \"y\") return \"mark\"\n\
    cell.slot = Token()\n\
    if (cell.slot === first) return \"slot\"\n\
    return \"OK\"\n\
}\n";

fn compiled_classes(stem: &str, source: &str) -> (Vec<(String, Vec<u8>)>, Vec<(String, Vec<u8>)>) {
    let work = common::scratch_dir().expect("allocate anonymous-object fixture");
    let source_path = work.join(format!("{stem}.kt"));
    let krusty_output = work.join("krusty");
    let reference_output = work.join("reference");
    std::fs::write(&source_path, source).expect("write anonymous-object fixture");
    std::fs::create_dir_all(&krusty_output).expect("create krusty output");
    std::fs::create_dir_all(&reference_output).expect("create reference output");
    let krusty = std::process::Command::new(common::krusty_binary())
        .args(["-d", krusty_output.to_str().expect("UTF-8 output")])
        .arg("-no-reflect")
        .arg(&source_path)
        .output()
        .expect("run krusty");
    assert!(
        krusty.status.success(),
        "{stem}: krusty failed: {}",
        String::from_utf8_lossy(&krusty.stderr)
    );
    let reference_args = vec![
        "-d".to_string(),
        reference_output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        source_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) =
        common::kotlinc_compile(&reference_args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");
    let read = |root: &Path| {
        let mut classes = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).expect("read compiler output") {
                let path = entry.expect("read compiler output entry").path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().and_then(|extension| extension.to_str()) == Some("class")
                {
                    let name = path
                        .strip_prefix(root)
                        .expect("class below output root")
                        .with_extension("")
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/");
                    classes.push((name, std::fs::read(&path).expect("read compiled class")));
                }
            }
        }
        classes.sort_by(|left, right| left.0.cmp(&right.0));
        classes
    };
    let compiled = (read(&reference_output), read(&krusty_output));
    let _ = std::fs::remove_dir_all(work);
    compiled
}

fn accessor_identity(
    info: &krusty::jvm::classreader::ClassInfo,
) -> (
    Vec<(String, String, bool)>,
    Vec<(String, Option<(String, String)>, Option<(String, String)>)>,
) {
    let mut methods = info
        .methods
        .iter()
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.is_bridge(),
            )
        })
        .collect::<Vec<_>>();
    methods.sort();
    let mut properties = info
        .meta
        .class_properties
        .iter()
        .filter_map(|property| {
            let getter = property
                .getter
                .as_ref()
                .map(|getter| (getter.name.clone(), getter.desc.clone()));
            let setter = property
                .setter
                .as_ref()
                .map(|setter| (setter.name.clone(), setter.desc.clone()));
            (getter.is_some() || setter.is_some()).then_some((
                property.name.clone(),
                getter,
                setter,
            ))
        })
        .collect::<Vec<_>>();
    properties.sort();
    (methods, properties)
}

#[test]
fn a_reified_anonymous_object_copies_property_accessors_like_kotlinc() {
    common::expect_box_same_as_kotlinc(PROPERTIES, "ReifiedAnonymousProperties");
    let (reference, krusty) = compiled_classes("ReifiedAnonymousProperties", PROPERTIES);
    let reference_names = reference
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let krusty_names = krusty
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    assert_eq!(krusty_names, reference_names);
    let inlined = reference_names
        .iter()
        .find(|name| name.contains("$$inlined$make$"))
        .expect("the call site has its own anonymous class")
        .clone();
    let reference_bytes = reference
        .iter()
        .find(|(name, _)| name == &inlined)
        .map(|(_, bytes)| bytes.as_slice())
        .expect("reference class");
    let krusty_bytes = krusty
        .iter()
        .find(|(name, _)| name == &inlined)
        .map(|(_, bytes)| bytes.as_slice())
        .expect("krusty class");
    let reference_info =
        krusty::jvm::classreader::parse_class(reference_bytes).expect("parse reference class");
    let krusty_info =
        krusty::jvm::classreader::parse_class(krusty_bytes).expect("parse krusty class");
    assert_eq!(
        accessor_identity(&krusty_info),
        accessor_identity(&reference_info),
        "{inlined} accessor, bridge, and metadata identities"
    );
}

#[test]
fn an_external_reified_call_copies_the_anonymous_class() {
    const SOURCE: &str = "\
interface Face { fun value(): String }\n\
class Token\n\
class Other\n\
inline fun <reified T> externalInline(): String = T::class.simpleName ?: \"none\"\n\
inline fun <reified T> make(): Face = object : Face {\n\
    override fun value() = externalInline<T>()\n\
}\n\
fun box(): String {\n\
    val face = make<Token>()\n\
    if (face.value() != \"Token\") return \"fail:${face.value()}\"\n\
    if (make<Other>().value() != \"Other\") return \"other\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SOURCE, "ReifiedAnonymousExternal");
    let (reference, krusty) = class_names("ReifiedAnonymousExternal", SOURCE);
    assert_eq!(krusty, reference);
    assert!(reference
        .iter()
        .any(|name| name.contains("$$inlined$make$")));
}

#[test]
fn a_local_delegate_plan_copies_the_anonymous_class() {
    const SOURCE: &str = "\
import kotlin.reflect.KProperty\n\
interface Face { fun value(): String }\n\
interface Item\n\
class Token : Item\n\
class Other : Item\n\
class Delegate<T>(val value: Any?)\n\
inline operator fun <reified T> Delegate<T>.getValue(owner: Any?, property: KProperty<*>): String =\n\
    if (value is T) \"yes\" else \"no\"\n\
inline fun <reified T : Item> make(value: Any?): Face = object : Face {\n\
    override fun value(): String {\n\
        val local by Delegate<T>(value)\n\
        return local\n\
    }\n\
}\n\
fun box(): String {\n\
    if (make<Token>(Token()).value() != \"yes\") return \"token\"\n\
    if (make<Token>(Other()).value() != \"no\") return \"other\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SOURCE, "ReifiedAnonymousDelegate");
    let (reference, krusty) = class_names("ReifiedAnonymousDelegate", SOURCE);
    assert_eq!(krusty, reference);
    assert!(reference
        .iter()
        .any(|name| name.contains("$$inlined$make$")));
}

#[test]
fn a_private_inline_anonymous_object_is_used_as_its_supertype() {
    // Two call sites each get a class. The values meet as the interface, not as the declaration
    // class, so neither copy is cast back to `...$f$1`.
    common::expect_box_same_as_kotlinc(
        r#"
interface I
class C<T>

private inline fun <reified T> C<T>.f() = object : I {
    val unused = T::class
}

fun box(): String {
    val t1 = C<String>().f()
    val t2 = C<String>().f()
    arrayOf(t1, t2)
    return "OK"
}
"#,
        "PrivateInlineAnonymousSupertype",
    );
}
