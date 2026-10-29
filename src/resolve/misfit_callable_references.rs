//! Callable references whose every function candidate rejects the expected function shape.
//!
//! kotlinc still resolves such a reference, and where the reference stands decides how the
//! failure reads. As a call's value argument the reference is postponed into that call, so its
//! candidates are reported: one as `inapplicable candidate(s)`, several as `none of the following
//! candidates is applicable` with each candidate's own reason. Anywhere else the expected type only
//! checks the reference after resolution: the nearest tower level with candidates decides, a
//! single candidate gives the reference its reflection type (`KFunction3<B, Int, Int, String>`)
//! for the enclosing initializer, return or assignment check, and several are listed.

use super::*;

/// The function candidates a callable reference names, in kotlinc's tower order: the receiver's
/// members, then its extensions, each level in declaration order.
pub(super) struct ReferenceCandidateLevels {
    members: Vec<crate::libraries::FunctionInfo>,
    extensions: Vec<crate::libraries::FunctionInfo>,
}

impl ReferenceCandidateLevels {
    /// The levels of `candidates`, or `None` when a property shares the name: a property
    /// candidate is not a function shape this rule describes.
    pub(super) fn of(candidates: &CallableRefCandidates) -> Option<Self> {
        if candidates.property.is_some() || candidates.extension_property.is_some() {
            return None;
        }
        let (members, extensions) = candidates
            .overloads
            .iter()
            .cloned()
            .partition::<Vec<_>, _>(|candidate| candidate.kind == crate::libraries::FnKind::Member);
        let levels = Self {
            members,
            extensions,
        };
        (!levels.members.is_empty() || !levels.extensions.is_empty()).then_some(levels)
    }

    fn nearest(&self) -> &[crate::libraries::FunctionInfo] {
        if self.members.is_empty() {
            &self.extensions
        } else {
            &self.members
        }
    }

    fn all(&self) -> impl Iterator<Item = &crate::libraries::FunctionInfo> {
        self.members.iter().chain(&self.extensions)
    }
}

/// How a misfit reference's failure is reported once its position is known.
pub(super) enum MisfitReference {
    /// The diagnostic is reported; the reference is an error.
    Reported,
    /// The reference resolves to its only nearest candidate without the expectation and takes
    /// that candidate's reflection type, so the enclosing check reports the mismatch.
    Natural,
}

impl Checker<'_> {
    /// Report the misfit reference `expression` to `name` over `levels`, or leave it to the
    /// enclosing check when it resolves to its only nearest candidate.
    pub(super) fn report_misfit_callable_reference(
        &mut self,
        expression: ExprId,
        name: &str,
        levels: &ReferenceCandidateLevels,
    ) -> MisfitReference {
        let message = if self.is_call_value_argument(expression) {
            let displays = levels
                .all()
                .map(|candidate| self.member_candidate_display(name, candidate))
                .collect::<Vec<_>>();
            match displays.as_slice() {
                [display] => format!("inapplicable candidate(s): {display}"),
                displays => {
                    let reasons = displays
                        .iter()
                        .map(|display| {
                            format!("{display}:\n  Inapplicable candidate(s): {display}")
                        })
                        .collect::<Vec<_>>();
                    format!("{INAPPLICABLE_OVERLOAD_PREFIX}\n\n{}", reasons.join("\n\n"))
                }
            }
        } else {
            match levels.nearest() {
                [_] => return MisfitReference::Natural,
                candidates => {
                    let displays = candidates
                        .iter()
                        .map(|candidate| self.member_candidate_display(name, candidate))
                        .collect::<Vec<_>>();
                    format!("{INAPPLICABLE_OVERLOAD_PREFIX}\n\n{}", displays.join("\n"))
                }
            }
        };
        if self.silent_error_exprs.insert(expression) {
            self.diags
                .error(self.member_name_span(expression, name), message);
        }
        MisfitReference::Reported
    }

    /// Whether `expression` is written directly as a value argument of a call: an ordinary or safe
    /// call, an enum entry's constructor call, a supertype constructor call or a constructor
    /// delegation. Only a failed reference asks, so the file is scanned instead of indexing every
    /// argument up front.
    fn is_call_value_argument(&self, expression: ExprId) -> bool {
        let in_expression = self
            .file
            .expr_arena
            .iter()
            .any(|candidate| match candidate {
                Expr::Call { args, .. } => args.contains(&expression),
                Expr::SafeCall {
                    args: Some(args), ..
                } => args.contains(&expression),
                _ => false,
            });
        in_expression
            || self.file.decl_arena.iter().any(|declaration| {
                let Decl::Class(class) = declaration else {
                    return false;
                };
                class.base_args.contains(&expression)
                    || class
                        .enum_entries
                        .iter()
                        .any(|entry| entry.args.contains(&expression))
                    || class.secondary_ctors.iter().any(|constructor| {
                        matches!(
                            &constructor.delegation,
                            CtorDelegation::This(call) | CtorDelegation::Super(call)
                                if call.args.contains(&expression)
                        )
                    })
            })
    }
}
