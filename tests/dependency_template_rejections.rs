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

/// The frontend rejection of a call of a required inline dependency function that has no
/// template. The callee's identity is opaque and assigned per compilation, so it is read back
/// whole from the message and must be a number. The `Body` and `UnsupportedExternalCall`
/// parentheses stay exact. When the provider still holds the selected declaration, the driver
/// appends that declaration after them; a missing selection leaves nothing after the parentheses.
pub(super) fn required_inline_rejection(message: &str) -> Option<u32> {
    let rest = message.strip_prefix(
        "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(",
    )?;
    let (id, tail) = rest.split_once(")))")?;
    let id = id.parse().ok()?;
    if tail.is_empty() {
        return Some(id);
    }
    let described = tail.strip_prefix("; selected dependency owner=")?;
    (described.contains(", name=")
        && described.contains(", declaration=")
        && described.contains(", dependency="))
    .then_some(id)
}

#[cfg(test)]
mod tests {
    use super::required_inline_rejection;

    #[test]
    fn a_required_inline_rejection_reads_the_opaque_id_and_its_selected_declaration() {
        let message = "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(88))); selected dependency owner=Some(Package(TypeName(\"\"))), name=\"countOf\", declaration=Some(Public(id)), dependency=Some(dep)";
        assert_eq!(required_inline_rejection(message), Some(88));
    }

    #[test]
    fn a_required_inline_rejection_without_a_retained_declaration_keeps_the_bare_id() {
        let message = "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(13)))";
        assert_eq!(required_inline_rejection(message), Some(13));
    }

    #[test]
    fn a_required_inline_rejection_rejects_a_rewritten_tail() {
        assert_eq!(required_inline_rejection("nope"), None);
        assert_eq!(
            required_inline_rejection(
                "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(no)))"
            ),
            None
        );
        assert_eq!(
            required_inline_rejection(
                "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(13))); something else"
            ),
            None
        );
    }
}
