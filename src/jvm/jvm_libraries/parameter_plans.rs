//! Physical parameter roles published while classfile entries are aligned with declarations.

use crate::libraries::{LibraryCallable, LibraryMember};

/// A top-level callable's provider-normalized vector has already had synthetic ABI tails removed.
pub(super) fn source_only(callable: &mut LibraryCallable) {
    callable.physical_parameter_plan = Some(
        crate::libraries::physical_parameter_plan::source_parameter_plan(
            callable.physical_params.len(),
        ),
    );
}

/// Publish the explicit ABI roles for a provider-built callable whose physical vector may retain
/// a dispatch receiver or continuation.
pub(super) fn callable(callable: &mut LibraryCallable, dispatch: bool, continuation: bool) {
    match crate::libraries::physical_parameter_plan::parameter_plan(
        callable.params.len(),
        callable.physical_params.len(),
        dispatch,
        continuation,
    ) {
        Ok(plan) => callable.physical_parameter_plan = Some(plan),
        Err(detail) => {
            callable.physical_parameter_plan = None;
            crate::trace_compiler!(
                "callable_slots",
                "rejected parameter alignment name={} {}",
                callable.name,
                detail
            );
        }
    }
}

/// Logical descriptor of a semantically suspend declaration from its exact physical CPS
/// descriptor. Provider normalization calls this once; an already-logical descriptor is not valid
/// input because it no longer contains the backend-owned continuation slot.
///
/// The selected declaration's suspend fact identifies the final ABI slot; no classifier spelling
/// is inspected. A malformed descriptor is left for the ordinary descriptor validator to reject.
pub(super) fn logical_suspend_descriptor(descriptor: &str) -> String {
    let Some((mut parameters, result)) = crate::jvm::names::parse_method_descriptor(descriptor)
    else {
        return descriptor.to_string();
    };
    if parameters.pop().is_none() {
        return descriptor.to_string();
    }
    descriptor_from_parts(&parameters, result)
}

/// Physical CPS descriptor for a semantically suspend declaration.
///
/// The continuation classifier is JVM suspend ABI, not a source-resolution fact. Keep that
/// representation spelling at this provider boundary.
pub(super) fn physical_suspend_descriptor(descriptor: &str) -> Option<String> {
    let (parameters, result) = crate::jvm::names::parse_method_descriptor(descriptor)?;
    let mut physical = descriptor_from_parts(&parameters, result);
    let result_at = physical.find(')')?;
    physical.insert_str(result_at, "Lkotlin/coroutines/Continuation;");
    Some(physical)
}

/// Reassemble components already validated by `parse_method_descriptor` without interpreting
/// their JVM representation as source-language types.
fn descriptor_from_parts(parameters: &[&str], result: &str) -> String {
    let capacity = parameters
        .iter()
        .map(|parameter| parameter.len())
        .sum::<usize>()
        + result.len()
        + 2;
    let mut descriptor = String::with_capacity(capacity);
    descriptor.push('(');
    for parameter in parameters {
        descriptor.push_str(parameter);
    }
    descriptor.push(')');
    descriptor.push_str(result);
    descriptor
}

/// Publish the explicit ABI roles established by metadata/classfile alignment.
///
/// An inconsistent declaration remains a candidate with no plan. If selected, realization reports
/// that exact invariant failure instead of resolution pretending the declaration did not exist.
pub(super) fn member(member: &mut LibraryMember, dispatch: bool, continuation: bool) {
    match crate::libraries::physical_parameter_plan::parameter_plan(
        member.params.len(),
        member.physical_params.len(),
        dispatch,
        continuation,
    ) {
        Ok(plan) => member.physical_parameter_plan = Some(plan),
        Err(detail) => {
            member.physical_parameter_plan = None;
            crate::trace_compiler!(
                "member_slots",
                "rejected parameter alignment name={} {}",
                member.name,
                detail
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{logical_suspend_descriptor, physical_suspend_descriptor};

    #[test]
    fn suspend_descriptors_preserve_validated_jvm_components() {
        assert_eq!(
            logical_suspend_descriptor("(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;"),
            "(I)Ljava/lang/Object;"
        );
        assert_eq!(
            physical_suspend_descriptor("(Lsample/Token;)Ljava/lang/Object;").as_deref(),
            Some("(Lsample/Token;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;")
        );
    }

    #[test]
    fn malformed_descriptors_remain_rejected() {
        assert_eq!(
            logical_suspend_descriptor("not-a-descriptor"),
            "not-a-descriptor"
        );
        assert_eq!(physical_suspend_descriptor("not-a-descriptor"), None);
    }
}
