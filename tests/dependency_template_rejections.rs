//! Platform-specific rejection contracts for required KLIB inline dependency templates.

#[derive(Clone, Copy)]
pub(super) struct Case {
    pub(super) stem: &'static str,
    pub(super) source: &'static str,
}

/// A library's reified inline function whose body the library serializes no template for (its
/// `for` loop). The declaration has no legal direct call, so a target that expands dependency
/// inline bodies in IR must reject the consumer rather than call it.
pub(super) const DECLINED_REQUIRED_INLINE_BODY: Case = Case {
    stem: "multimoduleDeclinedRequiredInlineBody",
    source: "// MODULE: lib\n\
             inline fun <reified T> countOf(value: Any, times: Int): Int {\n\
             \x20   var count = 0\n\
             \x20   for (index in 0 until times) if (value is T) count++\n\
             \x20   return count\n\
             }\n\
             // MODULE: main(lib)\n\
             fun box(): String = if (countOf<String>(\"a\", 2) == 2) \"OK\" else \"fail\"\n",
};

/// A required inline factory whose prepared body contains a rich function reference, which
/// template lowering does not yet support. Neither target may emit an illegal direct call to
/// the factory.
pub(super) const DECLINED_STDLIB_INLINE_BODY: &str = "fun box(): String {\n\
                                               \x20   val order = compareBy<String> { it }\n\
                                               \x20   return if (order.compare(\"a\", \"b\") < 0) \"OK\" else \"fail\"\n\
                                               }\n";

/// Establish that the platform-specific rejection fixture is an ordinary valid Kotlin program.
pub(super) fn verify_declined_stdlib_source_with_kotlinc() {
    let stdlib = super::common::stdlib_jar();
    let reference = super::box_reference_classes::reference_compile(
        DECLINED_STDLIB_INLINE_BODY,
        "declinedRequiredStdlibTemplate",
        std::slice::from_ref(&stdlib),
        krusty::conformance::BoxJdk::Full { root: None },
        "",
    )
    .expect("pinned kotlinc must compile the required-inline rejection fixture");
    let classes = reference.flatten().into_iter().collect::<Vec<_>>();
    let entry = super::common::find_box_class(&classes)
        .expect("the required-inline rejection fixture declares box()");
    assert_eq!(
        super::common::run_box(&classes, &entry, &[stdlib]),
        Some("OK".to_owned()),
        "pinned kotlinc must run the required-inline rejection fixture"
    );
}

/// The selected declaration a required inline dependency's rejection names, keyed as the producer
/// frames it. The checked callable id is opaque producer numbering: it must agree in all three
/// positions of the record and is not part of the identity. Each metadata type parameter
/// (`\0metadata-tp:<n>`) is renumbered `\0tp<k>` by order of first appearance, so the record
/// compares the declaration's type facts rather than one load's interning order.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct RequiredInlineRejection {
    pub(super) dependency: String,
    pub(super) declaration: String,
    pub(super) owner: String,
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) parameters: String,
    pub(super) result: String,
    pub(super) declared_frame: String,
    pub(super) inline: String,
    pub(super) role: String,
    pub(super) intrinsic: String,
}

/// Assert that a target rejected its caller with exactly the complete selected-declaration record
/// `expected` for the required inline dependency call.
#[track_caller]
pub(super) fn assert_required_inline_rejection(
    stem: &str,
    outcome: &super::box_lane::Outcome,
    expected: &RequiredInlineRejection,
) {
    let super::box_lane::Outcome::Frontend(message) = outcome else {
        panic!("{stem}: expected the required-inline rejection, got {outcome:?}");
    };
    let Some(actual) = required_inline_rejection(message) else {
        panic!("{stem}: malformed required-inline rejection: {message}");
    };
    assert_eq!(&actual, expected, "{stem}: {message}");
}

/// The rejection record of countOf<String> in [`DECLINED_REQUIRED_INLINE_BODY`]'s `lib` module.
pub(super) fn declined_count_of() -> RequiredInlineRejection {
    let signature = r#"Public(KlibPublicIdSignature { package: KlibNamePath { segments: [] }, declaration: KlibNamePath { segments: ["countOf"] }, member_id: Some(7723176677164391166), mask: 0 })"#;
    let parameters = r#"[Obj(TypeName("kotlin/Any"), []), Obj(TypeName("kotlin/Int"), [])]"#;
    let result = r#"Obj(TypeName("kotlin/Int"), [])"#;
    RequiredInlineRejection {
        dependency: format!(
            r#"Some(KlibDependencyCallable {{ module: "lib", declaration: {signature} }})"#
        ),
        declaration: format!("Some({signature})"),
        owner: r#"Some(Package(TypeName("")))"#.to_owned(),
        name: r#""countOf""#.to_owned(),
        kind: "TopLevel".to_owned(),
        parameters: parameters.to_owned(),
        result: result.to_owned(),
        declared_frame: format!(
            r#"(None, Some({parameters}), None, Some(GenericSig {{ formals: ["\0tp0"], formal_bounds: [[]], receiver: None, params: {parameters}, ret: {result}, return_policy: Exact }}))"#
        ),
        inline: "MustInline".to_owned(),
        role: "None".to_owned(),
        intrinsic: "None".to_owned(),
    }
}

/// The rejection record of the stdlib's compareBy in [`DECLINED_STDLIB_INLINE_BODY`], selected from
/// the target's stdlib KLIB, whose manifest names its module `stdlib_module`.
pub(super) fn declined_compare_by(stdlib_module: &str) -> RequiredInlineRejection {
    let signature = r#"Public(KlibPublicIdSignature { package: KlibNamePath { segments: ["kotlin", "comparisons"] }, declaration: KlibNamePath { segments: ["compareBy"] }, member_id: Some(14579530963861932892), mask: 0 })"#;
    let any = r#"Nullable(Obj(TypeName("kotlin/Any"), []))"#;
    let parameters = format!(
        r#"[Fun(FnSig {{ params: [TyParam("\0tp0", {any})], ret: Nullable(Obj(TypeName("kotlin/Comparable"), [StarProjection({any})])), context_count: 0, has_receiver: false, suspend: false }})]"#
    );
    let result = format!(r#"Obj(TypeName("kotlin/Comparator"), [TyParam("\0tp0", {any})])"#);
    RequiredInlineRejection {
        dependency: format!(
            r#"Some(KlibDependencyCallable {{ module: "{stdlib_module}", declaration: {signature} }})"#
        ),
        declaration: format!("Some({signature})"),
        owner: r#"Some(Package(TypeName("kotlin/comparisons")))"#.to_owned(),
        name: r#""compareBy""#.to_owned(),
        kind: "TopLevel".to_owned(),
        declared_frame: format!(
            r#"(None, Some({parameters}), None, Some(GenericSig {{ formals: ["\0tp0"], formal_bounds: [[]], receiver: None, params: {parameters}, ret: {result}, return_policy: Exact }}))"#
        ),
        parameters,
        result,
        inline: "MustInline".to_owned(),
        role: "None".to_owned(),
        intrinsic: "None".to_owned(),
    }
}

/// Parse the complete versioned selected-declaration rejection record. A bare rejection without
/// the record, a malformed frame, or a disagreeing checked id is not a rejection record.
pub(super) fn required_inline_rejection(message: &str) -> Option<RequiredInlineRejection> {
    let message = message.strip_prefix(
        "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(",
    )?;
    let (identity, tail) = message.split_once(")))")?;
    let identity = decimal(identity)?;
    let tail = tail.strip_prefix("; selected external ExternalCallableId(")?;
    let (selected, mut tail) = tail.split_once("): v1")?;
    if decimal(selected)? != identity {
        return None;
    }
    let mut values = [""; 12];
    for (key, value) in [
        "id",
        "dependency",
        "declaration",
        "owner",
        "name",
        "kind",
        "parameters",
        "result",
        "declared_frame",
        "inline",
        "role",
        "intrinsic",
    ]
    .into_iter()
    .zip(&mut values)
    {
        tail = tail
            .strip_prefix("; ")?
            .strip_prefix(key)?
            .strip_prefix('=')?;
        let (length, rest) = tail.split_once(':')?;
        let length = usize::try_from(decimal(length)?).ok()?;
        *value = rest.get(..length)?;
        if value.is_empty() {
            return None;
        }
        tail = rest.get(length..)?;
    }
    if !tail.is_empty() || decimal(values[0])? != identity {
        return None;
    }
    let mut formals = Vec::new();
    let mut renumber = |value: &str| renumber_type_parameters(value, &mut formals);
    Some(RequiredInlineRejection {
        dependency: renumber(values[1]),
        declaration: renumber(values[2]),
        owner: renumber(values[3]),
        name: values[4].to_owned(),
        kind: values[5].to_owned(),
        parameters: renumber(values[6]),
        result: renumber(values[7]),
        declared_frame: renumber(values[8]),
        inline: values[9].to_owned(),
        role: values[10].to_owned(),
        intrinsic: values[11].to_owned(),
    })
}

/// Renumber each `\0metadata-tp:<n>` as `\0tp<k>`, `k` being its order of first appearance in the
/// whole record.
fn renumber_type_parameters(value: &str, formals: &mut Vec<String>) -> String {
    const MARKER: &str = "\\0metadata-tp:";
    let mut renumbered = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find(MARKER) {
        renumbered.push_str(&rest[..start]);
        let digits = &rest[start + MARKER.len()..];
        let end = digits
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(digits.len());
        let formal = &digits[..end];
        let ordinal = match formals.iter().position(|known| known == formal) {
            Some(ordinal) => ordinal,
            None => {
                formals.push(formal.to_owned());
                formals.len() - 1
            }
        };
        renumbered.push_str(&format!("\\0tp{ordinal}"));
        rest = &digits[end..];
    }
    renumbered.push_str(rest);
    renumbered
}

fn decimal(value: &str) -> Option<u32> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())?
}

#[cfg(test)]
mod tests {
    use super::{declined_compare_by, declined_count_of, required_inline_rejection};

    // Complete selected-declaration values from the two actual Native rejection records.
    const COUNT_OF: &str = r#"internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(132))); selected external ExternalCallableId(132): v1; id=3:132; dependency=233:Some(KlibDependencyCallable { module: "lib", declaration: Public(KlibPublicIdSignature { package: KlibNamePath { segments: [] }, declaration: KlibNamePath { segments: ["countOf"] }, member_id: Some(7723176677164391166), mask: 0 }) }); declaration=178:Some(Public(KlibPublicIdSignature { package: KlibNamePath { segments: [] }, declaration: KlibNamePath { segments: ["countOf"] }, member_id: Some(7723176677164391166), mask: 0 })); owner=27:Some(Package(TypeName(""))); name=9:"countOf"; kind=8:TopLevel; parameters=66:[Obj(TypeName("kotlin/Any"), []), Obj(TypeName("kotlin/Int"), [])]; result=31:Obj(TypeName("kotlin/Int"), []); declared_frame=313:(None, Some([Obj(TypeName("kotlin/Any"), []), Obj(TypeName("kotlin/Int"), [])]), None, Some(GenericSig { formals: ["\0metadata-tp:3600"], formal_bounds: [[]], receiver: None, params: [Obj(TypeName("kotlin/Any"), []), Obj(TypeName("kotlin/Int"), [])], ret: Obj(TypeName("kotlin/Int"), []), return_policy: Exact })); inline=10:MustInline; role=4:None; intrinsic=4:None"#;
    const COMPARE_BY: &str = r#"internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(66))); selected external ExternalCallableId(66): v1; id=2:66; dependency=262:Some(KlibDependencyCallable { module: "stdlib", declaration: Public(KlibPublicIdSignature { package: KlibNamePath { segments: ["kotlin", "comparisons"] }, declaration: KlibNamePath { segments: ["compareBy"] }, member_id: Some(14579530963861932892), mask: 0 }) }); declaration=204:Some(Public(KlibPublicIdSignature { package: KlibNamePath { segments: ["kotlin", "comparisons"] }, declaration: KlibNamePath { segments: ["compareBy"] }, member_id: Some(14579530963861932892), mask: 0 })); owner=45:Some(Package(TypeName("kotlin/comparisons"))); name=11:"compareBy"; kind=8:TopLevel; parameters=266:[Fun(FnSig { params: [TyParam("\0metadata-tp:2165", Nullable(Obj(TypeName("kotlin/Any"), [])))], ret: Nullable(Obj(TypeName("kotlin/Comparable"), [StarProjection(Nullable(Obj(TypeName("kotlin/Any"), [])))])), context_count: 0, has_receiver: false, suspend: false })]; result=110:Obj(TypeName("kotlin/Comparator"), [TyParam("\0metadata-tp:2165", Nullable(Obj(TypeName("kotlin/Any"), [])))]); declared_frame=792:(None, Some([Fun(FnSig { params: [TyParam("\0metadata-tp:2165", Nullable(Obj(TypeName("kotlin/Any"), [])))], ret: Nullable(Obj(TypeName("kotlin/Comparable"), [StarProjection(Nullable(Obj(TypeName("kotlin/Any"), [])))])), context_count: 0, has_receiver: false, suspend: false })]), None, Some(GenericSig { formals: ["\0metadata-tp:2165"], formal_bounds: [[]], receiver: None, params: [Fun(FnSig { params: [TyParam("\0metadata-tp:2165", Nullable(Obj(TypeName("kotlin/Any"), [])))], ret: Nullable(Obj(TypeName("kotlin/Comparable"), [StarProjection(Nullable(Obj(TypeName("kotlin/Any"), [])))])), context_count: 0, has_receiver: false, suspend: false })], ret: Obj(TypeName("kotlin/Comparator"), [TyParam("\0metadata-tp:2165", Nullable(Obj(TypeName("kotlin/Any"), [])))]), return_policy: Exact })); inline=10:MustInline; role=4:None; intrinsic=4:None"#;

    #[test]
    fn complete_selected_contexts_are_the_selected_declarations() {
        assert_eq!(
            required_inline_rejection(COUNT_OF),
            Some(declined_count_of())
        );
        assert_eq!(
            required_inline_rejection(COMPARE_BY),
            Some(declined_compare_by("stdlib"))
        );
        // The opaque numbering is not identity: another load's ids name the same declaration.
        let renumbered = COUNT_OF
            .replace("ExternalCallableId(132)", "ExternalCallableId(7)")
            .replace("id=3:132", "id=1:7")
            .replace("declared_frame=313:", "declared_frame=310:")
            .replace("metadata-tp:3600", "metadata-tp:9");
        assert_eq!(
            required_inline_rejection(&renumbered),
            Some(declined_count_of())
        );
        let unicode = COUNT_OF.replace(r#"name=9:"countOf""#, r#"name=4:"é""#);
        let mut expected = declined_count_of();
        expected.name = r#""é""#.to_owned();
        assert_eq!(required_inline_rejection(&unicode), Some(expected));
    }

    #[test]
    fn another_selected_declaration_is_not_the_expected_record() {
        for (changed, from, to) in [
            ("kind", "kind=8:TopLevel", "kind=6:Member"),
            ("inline", "inline=10:MustInline", "inline=4:None"),
            ("name", r#"name=9:"countOf""#, r#"name=9:"countBy""#),
            (
                "module",
                r#"dependency=233:Some(KlibDependencyCallable { module: "lib""#,
                r#"dependency=233:Some(KlibDependencyCallable { module: "lip""#,
            ),
            (
                "parameters",
                r#"parameters=66:[Obj(TypeName("kotlin/Any"), []), Obj(TypeName("kotlin/Int"), [])]"#,
                r#"parameters=66:[Obj(TypeName("kotlin/Any"), []), Obj(TypeName("kotlin/Byt"), [])]"#,
            ),
            (
                "result",
                r#"result=31:Obj(TypeName("kotlin/Int"), [])"#,
                r#"result=31:Obj(TypeName("kotlin/Byt"), [])"#,
            ),
        ] {
            let other = COUNT_OF.replace(from, to);
            assert_ne!(other, COUNT_OF, "{changed}");
            let record = required_inline_rejection(&other).unwrap_or_else(|| panic!("{changed}"));
            assert_ne!(record, declined_count_of(), "{changed}");
        }
    }

    #[test]
    fn only_complete_records_are_rejection_records() {
        let bare = "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(132)))";
        for invalid in [
            bare.to_owned(),
            format!("{bare} trailing"),
            format!("{bare}; selected external ExternalCallableId(132): declaration unavailable"),
            COUNT_OF.replace("ExternalCallableId(132): v1", "ExternalCallableId(133): v1"),
            COUNT_OF.replacen("ExternalCallableId(132)", "ExternalCallableId(133)", 1),
            COUNT_OF.replacen("ExternalCallableId(132)", "ExternalCallableId(+132)", 1),
            COUNT_OF.replacen(
                "ExternalCallableId(132)",
                "ExternalCallableId(4294967296)",
                1,
            ),
            COUNT_OF.replace("id=3:132", "id=3:133"),
            COUNT_OF.replace(": v1;", ": v2;"),
            COUNT_OF.replace("; owner=", "; unknown="),
            COUNT_OF.replace(
                "; role=4:None; intrinsic=4:None",
                "; intrinsic=4:None; role=4:None",
            ),
            COUNT_OF.replace("; role=4:None", ""),
            COUNT_OF.replace("id=3:132", "id=0:"),
            COUNT_OF.replace("id=3:132", "id=999999999999999999999999:132"),
            COUNT_OF.replace("id=3:132", "id=+3:132"),
            COUNT_OF.replace("id=3:132", "id=4:132"),
            COUNT_OF.replace(
                "UnsupportedExternalCall",
                "UnsupportedExternalCallableReference",
            ),
            COUNT_OF.replace(r#"name=9:"countOf""#, r#"name=2:"é""#),
            format!("{COUNT_OF}; id=3:132"),
            format!("{COUNT_OF}x"),
        ] {
            assert_eq!(required_inline_rejection(&invalid), None, "{invalid}");
        }
    }
}
