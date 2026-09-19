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

    // The DECLARATION surface, compared against the reference compiler rather than pinned. A
    // top-level property of value-class type is realized over the CARRIER — `private static int`,
    // `getTopLevel()I`, `setTopLevel-<hash>(int)` — and the reference follows the declaration it
    // calls. Keeping it boxed here is what made the mangled reference name a method nothing had.
    let source = "@JvmInline\n\
                  value class Z(val x: Int)\n\
                  \n\
                  var topLevel: Z = Z(0)\n\
                  val readOnly: Z = Z(9)\n\
                  \n\
                  fun read(z: Z = topLevel) = z.x\n";
    let ours = krusty_dump("ValueClassTopLevelDecl", source);
    let reference = kotlinc_dump(
        "ValueClassTopLevelDeclRef",
        source,
        "ValueClassTopLevelDeclRefKt",
    );
    let surface = |dump: &str| {
        method_headers(dump)
            .into_iter()
            .filter(|header| header.contains("TopLevel") || header.contains("ReadOnly"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        surface(
            ours.get("ValueClassTopLevelDeclKt")
                .expect("krusty emits the facade")
        ),
        surface(&reference),
        "the top-level accessors are kotlinc's"
    );
    assert_eq!(
        surface(&reference),
        vec![
            "public static final int getTopLevel()".to_string(),
            "public static final void setTopLevel-IQRRRT4(int)".to_string(),
            "public static final int getReadOnly()".to_string(),
        ],
        "spelled out, so a reference change is visible here"
    );

    // The STORAGE those accessors read, and one reader of it. The field descriptor is the decision
    // the accessor names follow, and a defaulted parameter is the reader that exposes a disagreement
    // between the two: while the declaration said `LZ;` and the field said `I`, the default-argument
    // path boxed the carrier back into an `Integer` and cast it to `Z`.
    let ours_facade = ours
        .get("ValueClassTopLevelDeclKt")
        .expect("krusty emits the facade");
    assert_eq!(
        field_lines(ours_facade),
        field_lines(&reference),
        "the top-level storage is kotlinc's"
    );
    assert_eq!(
        field_lines(&reference),
        vec![
            "private static int topLevel".to_string(),
            "private static final int readOnly".to_string(),
        ],
        "spelled out, so a storage change is visible here"
    );
    assert_eq!(
        method_body(ours_facade, "read-IQRRRT4$default"),
        method_body(&reference, "read-IQRRRT4$default"),
        "a defaulted read of the property exchanges the carrier, as kotlinc's does"
    );
}

/// The field declarations of one `javap -p -c` dump, in order.
fn field_lines(dump: &str) -> Vec<String> {
    dump.lines()
        .map(str::trim)
        .filter(|line| line.ends_with(';') && !line.contains('(') && !line.contains('='))
        .filter(|line| line.starts_with("private ") || line.starts_with("public "))
        .map(|line| line.trim_end_matches(';').to_string())
        .collect()
}

/// The instruction mnemonics + operands of one method of a `javap -c` dump, constant-pool indices
/// dropped so two compilers' bodies compare by what they do.
fn method_body(dump: &str, name: &str) -> Vec<String> {
    let mut body = Vec::new();
    let mut inside = false;
    for line in dump.lines() {
        let trimmed = line.trim();
        let instruction_index = trimmed
            .split_once(": ")
            .is_some_and(|(index, _)| index.parse::<u32>().is_ok());
        if trimmed.ends_with(';') && !instruction_index {
            // Every declaration in the dump ends the previous one's body, `static {};` included.
            inside = trimmed.contains(name);
            continue;
        }
        if !inside || !instruction_index {
            continue;
        }
        let (index, rest) = trimmed.split_once(": ").expect("an indexed instruction");
        let instruction = rest.split_whitespace().next().unwrap_or_default();
        let operand = rest
            .split_once("// ")
            .map(|(_, comment)| comment.trim().to_string())
            .or_else(|| {
                rest.split_once(char::is_whitespace)
                    .map(|(_, operand)| operand.trim().to_string())
                    .filter(|operand| !operand.is_empty())
            })
            .unwrap_or_default();
        body.push(
            format!("{index}: {instruction} {operand}")
                .trim_end()
                .to_string(),
        );
    }
    assert!(!body.is_empty(), "no body for {name}");
    body
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

/// The same dump from the REFERENCE compiler, so a declaration ABI can be compared rather than
/// pinned: `javap -p` of one class it produced.
fn kotlinc_dump(stem: &str, source: &str, class: &str) -> String {
    let dir = common::scratch_dir().expect("scratch dir");
    let out = dir.join(format!("{stem}-ref"));
    std::fs::create_dir_all(&out).expect("reference dir");
    let source_path = dir.join(format!("{stem}.kt"));
    std::fs::write(&source_path, source).expect("write source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc unavailable under the test harness");
    assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");
    common::javap(&["-p", "-c", "-cp", &out.to_string_lossy(), class])
        .unwrap_or_else(|| panic!("{stem}: javap failed for {class}"))
}

/// Every method header of a dumped class, normalised to `<flags> <name>(<params>)`.
fn method_headers(dump: &str) -> Vec<String> {
    dump.lines()
        .map(str::trim)
        .filter(|line| line.ends_with(");") || line.ends_with(";") && line.contains('('))
        .filter(|line| line.contains('('))
        .map(|line| line.trim_end_matches(';').to_string())
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
    // The DECLARATION side, compared rather than pinned. kotlinc keeps a private member's accessor
    // private and publishes a synthetic bridge beside it; krusty used to publish the accessor
    // itself, so the reference linked only because the declaration was wrong. Both `Z` surfaces are
    // compared, so neither half can drift alone.
    let reference_z = kotlinc_dump("PrivateValueClassRefRef", source, "Z");
    let accessor_surface = |dump: &str| {
        method_headers(dump)
            .into_iter()
            .filter(|header| header.contains("getXx"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        accessor_surface(dump.get("Z").expect("krusty emits Z")),
        accessor_surface(&reference_z),
        "the private accessor and its bridge are kotlinc's"
    );
    assert_eq!(
        accessor_surface(&reference_z),
        vec![
            "private static final int getXx-impl(int)".to_string(),
            "public static final int access$getXx-impl(int)".to_string(),
        ],
        "spelled out, so a reference change is visible here"
    );
    assert_eq!(
        reference_get(&dump, "aload_0", "Method Z.\"access$getXx-impl\""),
        vec![
            "aload_0".to_string(),
            "getfield Field kotlin/jvm/internal/PropertyReference0Impl.receiver:Ljava/lang/Object;"
                .to_string(),
            "checkcast class Z".to_string(),
            "invokevirtual Method Z.\"unbox-impl\":()I".to_string(),
            "invokestatic Method Z.\"access$getXx-impl\":(I)I".to_string(),
            "invokestatic Method java/lang/Integer.valueOf:(I)Ljava/lang/Integer;".to_string(),
            "areturn".to_string(),
        ],
        "bound private value-class reference"
    );
    assert_eq!(
        reference_get(&dump, "aload_1", "Method Z.\"access$getXx-impl\""),
        vec![
            "aload_1".to_string(),
            "checkcast class Z".to_string(),
            "invokevirtual Method Z.\"unbox-impl\":()I".to_string(),
            "invokestatic Method Z.\"access$getXx-impl\":(I)I".to_string(),
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
