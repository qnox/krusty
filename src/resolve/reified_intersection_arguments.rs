//! The intersection case of kotlinc's `FirReifiedChecker`: a type argument for a reified type
//! parameter, written or inferred, that is an intersection type (`TYPE_INTERSECTION_AS_REIFIED`).
//! Reification erases it to a common supertype. kotlinc declares the diagnostic as a deprecation of
//! `ProhibitIntersectionReifiedTypeParameter`: an error with the feature, a warning without it.
//! An argument that is an array type is checked through its element types, as kotlinc does.

use super::*;

/// The name `@Suppress` silences the warning by: kotlinc names a deprecation's two severities
/// apart, so the bare diagnostic name suppresses neither.
const INTERSECTION_AS_REIFIED_WARNING: &str = "TYPE_INTERSECTION_AS_REIFIED_WARNING";

impl Checker<'_> {
    /// Report each reified type argument of the call `call` that is an intersection type, at the
    /// callee's name.
    pub(super) fn check_reified_intersection_arguments(&mut self, call: ExprId) {
        if self.discover_anonymous_captures {
            return;
        }
        let Some(arguments) = self.resolved_call_type_args.get(&call) else {
            return;
        };
        let Some(selected) = self.resolved_calls.get(&call) else {
            return;
        };
        let Some((formals, reified)) = reified_type_parameters(selected) else {
            return;
        };
        let mut reports = Vec::new();
        for &ordinal in reified {
            let Some(Some(argument)) = arguments.get(ordinal as usize) else {
                continue;
            };
            let Some(formal) = formals.get(ordinal as usize) else {
                continue;
            };
            let mut intersections = Vec::new();
            reified_intersections(*argument, &mut intersections);
            for parts in intersections {
                reports.push((formal.clone(), parts));
            }
        }
        if reports.is_empty() {
            return;
        }
        let gate = self
            .file
            .language_gates
            .intersection_reified_type_parameter
            .clone();
        if !gate.is_error() && self.suppresses_diagnostic(INTERSECTION_AS_REIFIED_WARNING) {
            return;
        }
        let span = self.call_callee_name_span(call);
        for (formal, parts) in reports {
            let rendered = parts
                .iter()
                .map(|part| self.diagnostic_type_name(*part, &[]))
                .collect::<Vec<_>>()
                .join("' & '");
            let message = gate.message(&format!(
                "type argument for reified type parameter '{}' was inferred to the intersection of \
                 ['{rendered}']. Reification of an intersection type results in the common \
                 supertype being used. This may lead to subtle issues and an explicit type \
                 argument is encouraged.",
                crate::types::type_parameter_source_name(&formal)
            ));
            if gate.is_error() {
                self.diags.error(span, message);
            } else {
                self.diags.warning(span, message);
            }
        }
    }
}

/// The selected callable's type-parameter names and the ordinals of its reified ones.
fn reified_type_parameters(selected: &ResolvedCall) -> Option<(&[String], &[u32])> {
    let (generic_sig, reified) = match selected {
        ResolvedCall::Member(resolved) => (
            resolved.member.generic_sig.as_ref(),
            resolved
                .member
                .call_sig
                .reified_type_parameter_ordinals
                .as_slice(),
        ),
        ResolvedCall::Companion(member) => (
            member.generic_sig.as_ref(),
            member.call_sig.reified_type_parameter_ordinals.as_slice(),
        ),
        ResolvedCall::TopLevel(call) => (
            call.callable.generic_sig.as_deref(),
            &call.callable.reified_type_parameter_ordinals[..],
        ),
        ResolvedCall::Extension(extension) => (
            extension.callable.generic_sig.as_deref(),
            &extension.callable.reified_type_parameter_ordinals[..],
        ),
        ResolvedCall::MemberExtension { .. } | ResolvedCall::LocalFunction(_) => return None,
    };
    (!reified.is_empty()).then_some(())?;
    Some((generic_sig?.formals.as_slice(), reified))
}

/// The intersection types `argument` reifies: itself, or an array's invariant element type at any
/// depth (a projected element is not checked, as kotlinc checks only type arguments that are types).
fn reified_intersections(argument: Ty, found: &mut Vec<&'static [Ty]>) {
    if let Ty::Intersection(parts) = argument {
        found.push(parts);
        return;
    }
    if let Ty::Obj(classifier, arguments) = argument.non_null() {
        if classifier.matches("kotlin/Array") {
            for &element in arguments {
                if element.projection_inner().is_none() {
                    reified_intersections(element, found);
                }
            }
        }
    }
}
