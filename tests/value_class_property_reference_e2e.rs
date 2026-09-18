//! A property reference whose RECEIVER is a value class.
//!
//! A value class's members are realized statically over its erased carrier — `Z.getXx-impl(I)I`,
//! and `ExtKt.getXx-IQRRRT4(I)I` for an extension on one. The property-reference class named an
//! ordinary instance accessor instead (`Z.getXx()I`), which is declared nowhere, so every one of
//! these programs failed at its first `get` with a `NoSuchMethodError` — an artifact that could not
//! link, emitted without a diagnostic.
//!
//! A second defect sat beside it: a property whose TYPE is a value class already had its accessor
//! NAME mangled, but a member or top-level property has no written descriptor, so the one the
//! emitter synthesized from the property's semantic type (`()LZ;`) named a method the declaration
//! does not have either — it returns the carrier, `()I`.
//!
//! Each case here is a corpus shape (`codegen/box/inlineClasses/callableReferences/`) reduced to
//! its smallest form, and every emitted body was compared against the reference compiler's.

use super::common;

fn run(source: &str, stem: &str) -> String {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::compile_and_run_box(source, stem, &[stdlib], Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("{stem} must compile and run"))
}

/// A MEMBER of a value class: the accessor is `Z.getXx-impl(I)I`, so the reference unboxes its
/// receiver and calls it statically — byte-identical to the reference compiler's
/// `checkcast Z; unbox-impl; invokestatic getXx-impl`.
#[test]
fn a_reference_to_a_member_of_a_value_class_resolves() {
    assert_eq!(
        run(
            "@JvmInline\n\
             value class Z(val x: Int) {\n\
             \x20   val xx get() = x\n\
             }\n\
             \n\
             @JvmInline\n\
             value class S(val x: String) {\n\
             \x20   val xx get() = x\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   if ((Z::xx).get(Z(42)) != 42) return \"FAIL1\"\n\
             \x20   if ((S::xx).get(S(\"ab\")) != \"ab\") return \"FAIL2\"\n\
             \x20   if (Z(7)::xx.get() != 7) return \"FAIL3\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ValueClassMemberRef",
        ),
        "OK"
    );
}

/// An EXTENSION on a value class: the accessor is the facade's hash-mangled
/// `getXx-IQRRRT4(I)I`, not `getXx(LZ;)I`. Its receiver is unboxed the same way.
#[test]
fn a_reference_to_an_extension_on_a_value_class_resolves() {
    assert_eq!(
        run(
            "@JvmInline\n\
             value class Z(val x: Int)\n\
             \n\
             val Z.xx get() = x + 1\n\
             \n\
             fun box(): String {\n\
             \x20   if ((Z::xx).get(Z(41)) != 42) return \"FAIL1\"\n\
             \x20   if (Z(1)::xx.get() != 2) return \"FAIL2\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ValueClassExtensionRef",
        ),
        "OK"
    );
}

/// The value class's own UNDERLYING property is the exception: reading it is the unbox, and its
/// accessor stays an ordinary instance getter on the box (`Z.getX()I`). A reference to it must not
/// be rewritten — doing so named `Z.getX-impl(I)I`, which does not exist.
#[test]
fn a_reference_to_a_value_classs_underlying_property_resolves() {
    assert_eq!(
        run(
            "@JvmInline\n\
             value class Z(val x: Int)\n\
             \n\
             @JvmInline\n\
             value class S(val x: String)\n\
             \n\
             fun box(): String {\n\
             \x20   if ((Z::x).get(Z(42)) != 42) return \"FAIL1\"\n\
             \x20   if ((S::x).get(S(\"ab\")) != \"ab\") return \"FAIL2\"\n\
             \x20   if (Z(7)::x.get() != 7) return \"FAIL3\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ValueClassUnderlyingRef",
        ),
        "OK"
    );
}

/// A property whose TYPE is a value class: its accessor exchanges the CARRIER (`C.getZ-a_XrcN0()I`,
/// `C.setZ-IQRRRT4(I)V`), which the synthesized descriptor has to say — it used to claim `()LZ;`.
#[test]
fn a_reference_to_a_value_class_typed_member_resolves() {
    assert_eq!(
        run(
            "@JvmInline\n\
             value class Z(val x: Int)\n\
             \n\
             class C(var z: Z)\n\
             \n\
             fun box(): String {\n\
             \x20   val ref = C::z\n\
             \x20   val c = C(Z(42))\n\
             \x20   if (ref.get(c).x != 42) return \"FAIL1\"\n\
             \x20   ref.set(c, Z(1234))\n\
             \x20   if (ref.get(c).x != 1234) return \"FAIL2\"\n\
             \x20   if (c::z.get().x != 1234) return \"FAIL3\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ValueClassTypedMemberRef",
        ),
        "OK"
    );
}

/// A TOP-LEVEL property of value-class type stays on the BOXED convention here, because its
/// backing field and accessors do (unlike the reference compiler's, which erase them — a separate
/// declaration-side item). Mangling the reference's accessor while the declaration kept its plain
/// name is what made this shape unlinkable, so the reference must stay on the same convention as
/// the declaration it calls.
#[test]
fn a_reference_to_a_value_class_typed_top_level_property_resolves() {
    assert_eq!(
        run(
            "@JvmInline\n\
             value class Z(val x: Int)\n\
             \n\
             var topLevel: Z = Z(0)\n\
             val readOnly: Z = Z(9)\n\
             \n\
             fun box(): String {\n\
             \x20   val ref = ::topLevel\n\
             \x20   ref.set(Z(42))\n\
             \x20   if (ref.get().x != 42) return \"FAIL1\"\n\
             \x20   if ((::readOnly).get().x != 9) return \"FAIL2\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ValueClassTopLevelRef",
        ),
        "OK"
    );
}

/// `javap -c` of every class krusty emits for one source, keyed by internal name.
fn krusty_dump(stem: &str, source: &str) -> std::collections::BTreeMap<String, String> {
    let dir = common::scratch_dir().expect("scratch dir");
    let out = dir.join(format!("{stem}-out"));
    std::fs::create_dir_all(&out).expect("output dir");
    let classes = common::compile_in_process_metadata_cp(source, stem, &[common::stdlib_jar()])
        .unwrap_or_else(|| panic!("{stem}: krusty failed to compile"));
    let mut names = Vec::new();
    for (name, bytes) in &classes {
        let path = out.join(format!("{name}.class"));
        std::fs::create_dir_all(path.parent().expect("class parent")).expect("class dir");
        std::fs::write(&path, bytes).expect("write class");
        names.push(name.clone());
    }
    names
        .into_iter()
        .map(|name| {
            let text = common::javap(&["-p", "-c", "-cp", &out.to_string_lossy(), &name])
                .unwrap_or_else(|| panic!("{stem}: javap failed for {name}"));
            (name, text)
        })
        .collect()
}

/// The instructions of the `get` override of the one reference class whose body starts with
/// `first` and mentions `needle`.
fn reference_get(
    dump: &std::collections::BTreeMap<String, String>,
    first: &str,
    needle: &str,
) -> Vec<String> {
    let mut found: Vec<Vec<String>> = Vec::new();
    for text in dump.values() {
        let mut body = Vec::new();
        let mut inside = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("public java.lang.Object get(") {
                inside = true;
                body.clear();
                continue;
            }
            if !inside {
                continue;
            }
            let Some((offset, rest)) = trimmed.split_once(": ") else {
                if trimmed.is_empty() {
                    inside = false;
                }
                continue;
            };
            if !offset.chars().all(|c| c.is_ascii_digit()) || offset.is_empty() {
                continue;
            }
            let row = match rest.split_once("// ") {
                Some((code, comment)) => format!(
                    "{} {}",
                    code.split_whitespace().next().unwrap_or(""),
                    comment.trim()
                ),
                None => rest.split_whitespace().collect::<Vec<_>>().join(" "),
            };
            body.push(row);
        }
        if body.first().is_some_and(|row| row == first)
            && body.iter().any(|row| row.contains(needle))
        {
            found.push(body);
        }
    }
    assert_eq!(
        found.len(),
        1,
        "exactly one reference {first:?} calling {needle:?}"
    );
    found.into_iter().next().expect("reference body")
}

/// A PRIVATE member of a value class, bound and unbound. The reference's accessor is selected as a
/// private-access one, and `ext_facade` is `Some` for that as well as for an extension — so reading
/// it as "this is an extension" rebuilt the hash-mangled `getXx-IQRRRT4(I)I` for a member whose
/// declaration is `getXx-impl(I)I`, and the program failed at its first `get`:
///
/// ```text
/// NoSuchMethodError: 'int Z.getXx-IQRRRT4(int)'
/// ```
///
/// The role the reference RECORDED decides it now. The ledger is asserted because a run alone
/// cannot tell a correct static realization from a lucky one.
#[test]
fn a_reference_to_a_private_member_of_a_value_class_resolves() {
    let source = "@JvmInline\n\
                  value class Z(val x: Int) {\n\
                  \x20   private val xx get() = x + 1\n\
                  \x20   fun bound(): Int = (this::xx).get()\n\
                  \x20   fun unbound(): Int = (Z::xx).get(Z(9))\n\
                  }\n\
                  class Ord(private val p: Int) {\n\
                  \x20   fun bound(): Int = (this::p).get()\n\
                  }\n\
                  fun box(): String {\n\
                  \x20   if (Z(41).bound() != 42) return \"FAIL1\"\n\
                  \x20   if (Z(0).unbound() != 10) return \"FAIL2\"\n\
                  \x20   if (Ord(5).bound() != 5) return \"FAIL3\"\n\
                  \x20   return \"OK\"\n\
                  }\n";
    assert_eq!(run(source, "PrivateValueClassRef"), "OK");

    let dump = krusty_dump("PrivateValueClassRefDump", source);
    assert_eq!(
        reference_get(&dump, "aload_0", "Method Z.\"getXx-impl\""),
        vec![
            "aload_0".to_string(),
            "getfield Field kotlin/jvm/internal/PropertyReference0Impl.receiver:Ljava/lang/Object;"
                .to_string(),
            "checkcast class Z".to_string(),
            "invokevirtual Method Z.\"unbox-impl\":()I".to_string(),
            "invokestatic Method Z.\"getXx-impl\":(I)I".to_string(),
            "invokestatic Method java/lang/Integer.valueOf:(I)Ljava/lang/Integer;".to_string(),
            "areturn".to_string(),
        ],
        "bound private value-class reference"
    );
    assert_eq!(
        reference_get(&dump, "aload_1", "Method Z.\"getXx-impl\""),
        vec![
            "aload_1".to_string(),
            "checkcast class Z".to_string(),
            "invokevirtual Method Z.\"unbox-impl\":()I".to_string(),
            "invokestatic Method Z.\"getXx-impl\":(I)I".to_string(),
            "invokestatic Method java/lang/Integer.valueOf:(I)Ljava/lang/Integer;".to_string(),
            "areturn".to_string(),
        ],
        "unbound private value-class reference"
    );
    // An ORDINARY class's private member keeps the `access$…$p` bridge it already had: the role is
    // recorded, not inferred, so naming the access-bridge case did not disturb it.
    assert_eq!(
        reference_get(&dump, "aload_0", "access$getP$p"),
        vec![
            "aload_0".to_string(),
            "getfield Field kotlin/jvm/internal/PropertyReference0Impl.receiver:Ljava/lang/Object;"
                .to_string(),
            "checkcast class Ord".to_string(),
            "invokestatic Method Ord.access$getP$p:(LOrd;)I".to_string(),
            "invokestatic Method java/lang/Integer.valueOf:(I)Ljava/lang/Integer;".to_string(),
            "areturn".to_string(),
        ],
        "an ordinary class's private reference is unchanged"
    );
}
