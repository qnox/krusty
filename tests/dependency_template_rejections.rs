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

/// Accept the exact bare rejection or its complete versioned selected-declaration context.
/// The checked identity is opaque, but must agree in all three positions in a context record.
pub(super) fn required_inline_rejection(message: &str) -> Option<u32> {
    let message = message.strip_prefix(
        "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(",
    )?;
    let (identity, tail) = message.split_once(")))")?;
    let identity = decimal(identity)?;
    if tail.is_empty() {
        return Some(identity);
    }
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
    (tail.is_empty()
        && decimal(values[0])? == identity
        && matches!(values[5], "TopLevel" | "Extension" | "Member")
        && values[9] == "MustInline")
        .then_some(identity)
}

fn decimal(value: &str) -> Option<u32> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())?
}

#[cfg(test)]
mod tests {
    use super::required_inline_rejection;

    // Complete selected-declaration values from the two actual Native rejection records.
    const COUNT_OF: &str = r#"internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(132))); selected external ExternalCallableId(132): v1; id=3:132; dependency=233:Some(KlibDependencyCallable { module: "lib", declaration: Public(KlibPublicIdSignature { package: KlibNamePath { segments: [] }, declaration: KlibNamePath { segments: ["countOf"] }, member_id: Some(7723176677164391166), mask: 0 }) }); declaration=178:Some(Public(KlibPublicIdSignature { package: KlibNamePath { segments: [] }, declaration: KlibNamePath { segments: ["countOf"] }, member_id: Some(7723176677164391166), mask: 0 })); owner=27:Some(Package(TypeName(""))); name=9:"countOf"; kind=8:TopLevel; parameters=66:[Obj(TypeName("kotlin/Any"), []), Obj(TypeName("kotlin/Int"), [])]; result=31:Obj(TypeName("kotlin/Int"), []); declared_frame=313:(None, Some([Obj(TypeName("kotlin/Any"), []), Obj(TypeName("kotlin/Int"), [])]), None, Some(GenericSig { formals: ["\0metadata-tp:3600"], formal_bounds: [[]], receiver: None, params: [Obj(TypeName("kotlin/Any"), []), Obj(TypeName("kotlin/Int"), [])], ret: Obj(TypeName("kotlin/Int"), []), return_policy: Exact })); inline=10:MustInline; role=4:None; intrinsic=4:None"#;
    const COMPARE_BY: &str = r#"internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(66))); selected external ExternalCallableId(66): v1; id=2:66; dependency=262:Some(KlibDependencyCallable { module: "stdlib", declaration: Public(KlibPublicIdSignature { package: KlibNamePath { segments: ["kotlin", "comparisons"] }, declaration: KlibNamePath { segments: ["compareBy"] }, member_id: Some(14579530963861932892), mask: 0 }) }); declaration=204:Some(Public(KlibPublicIdSignature { package: KlibNamePath { segments: ["kotlin", "comparisons"] }, declaration: KlibNamePath { segments: ["compareBy"] }, member_id: Some(14579530963861932892), mask: 0 })); owner=45:Some(Package(TypeName("kotlin/comparisons"))); name=11:"compareBy"; kind=8:TopLevel; parameters=266:[Fun(FnSig { params: [TyParam("\0metadata-tp:2165", Nullable(Obj(TypeName("kotlin/Any"), [])))], ret: Nullable(Obj(TypeName("kotlin/Comparable"), [StarProjection(Nullable(Obj(TypeName("kotlin/Any"), [])))])), context_count: 0, has_receiver: false, suspend: false })]; result=110:Obj(TypeName("kotlin/Comparator"), [TyParam("\0metadata-tp:2165", Nullable(Obj(TypeName("kotlin/Any"), [])))]); declared_frame=792:(None, Some([Fun(FnSig { params: [TyParam("\0metadata-tp:2165", Nullable(Obj(TypeName("kotlin/Any"), [])))], ret: Nullable(Obj(TypeName("kotlin/Comparable"), [StarProjection(Nullable(Obj(TypeName("kotlin/Any"), [])))])), context_count: 0, has_receiver: false, suspend: false })]), None, Some(GenericSig { formals: ["\0metadata-tp:2165"], formal_bounds: [[]], receiver: None, params: [Fun(FnSig { params: [TyParam("\0metadata-tp:2165", Nullable(Obj(TypeName("kotlin/Any"), [])))], ret: Nullable(Obj(TypeName("kotlin/Comparable"), [StarProjection(Nullable(Obj(TypeName("kotlin/Any"), [])))])), context_count: 0, has_receiver: false, suspend: false })], ret: Obj(TypeName("kotlin/Comparator"), [TyParam("\0metadata-tp:2165", Nullable(Obj(TypeName("kotlin/Any"), [])))]), return_policy: Exact })); inline=10:MustInline; role=4:None; intrinsic=4:None"#;

    #[test]
    fn complete_selected_contexts_preserve_required_inline_rejections() {
        assert_eq!(required_inline_rejection(COUNT_OF), Some(132));
        assert_eq!(required_inline_rejection(COMPARE_BY), Some(66));
        for kind in ["Extension", "Member"] {
            let member =
                COUNT_OF.replace("kind=8:TopLevel", &format!("kind={}:{kind}", kind.len()));
            assert_eq!(required_inline_rejection(&member), Some(132));
        }
        let unicode = COUNT_OF.replace(r#"name=9:"countOf""#, r#"name=4:"é""#);
        assert_eq!(required_inline_rejection(&unicode), Some(132));
    }

    #[test]
    fn bare_contract_is_exact_and_never_recovers_malformed_contexts() {
        let bare = "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(132)))";
        assert_eq!(required_inline_rejection(bare), Some(132));
        for invalid in [
            format!("{bare} trailing"),
            format!("{bare}; selected external ExternalCallableId(132): declaration unavailable"),
            bare.replace("UnsupportedExternalCall", "UnsupportedExternalConstructor"),
            bare.replace("132", "+132"),
            bare.replace("132", "4294967296"),
            bare.replace("132", ""),
            COUNT_OF.replace("ExternalCallableId(132): v1", "ExternalCallableId(133): v1"),
            COUNT_OF.replacen("ExternalCallableId(132)", "ExternalCallableId(133)", 1),
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
            COUNT_OF.replace("kind=8:TopLevel", "kind=11:Constructor"),
            COUNT_OF.replace("kind=8:TopLevel", "kind=7:Unknown"),
            COUNT_OF.replace(
                "UnsupportedExternalCall",
                "UnsupportedExternalCallableReference",
            ),
            COUNT_OF.replace("inline=10:MustInline", "inline=4:None"),
            COUNT_OF.replace(r#"name=9:"countOf""#, r#"name=2:"é""#),
            format!("{COUNT_OF}; id=3:132"),
            format!("{COUNT_OF}x"),
        ] {
            assert_eq!(required_inline_rejection(&invalid), None, "{invalid}");
        }
    }
}
