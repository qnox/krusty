//! Authenticate the semantic operand roles of an exact, already-emitted dependency export.
//! Instantiated checked types prove selection; targets separately retain the declaration-owned ABI.

use crate::types::Ty;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DependencyExportCall {
    TopLevel,
    /// An ordinary instance member whose exported ABI keeps its dispatch receiver first.
    Member,
    /// A member extension whose ABI keeps the class dispatch receiver first and the source
    /// extension receiver at its declared parameter coordinate.
    MemberExtension,
    /// A singleton member body emitted as a receiver-erased support export. The call still
    /// evaluates its checked receiver expression before invoking that export.
    SingletonMember,
    SourceExtension {
        receiver: Ty,
        receiver_position: usize,
    },
    SourceSingletonExtension {
        receiver: Ty,
        receiver_position: usize,
    },
    ClassifierAssociated {
        receiver: Ty,
        receiver_position: usize,
    },
    SerializedClassifierAssociated {
        receiver: Ty,
    },
    SerializedExtension,
}

/// The declaration-owned operand roles a defaulted dependency call is checked against.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeclaredOperandRoles<'a> {
    /// Leading context operands, which a call always supplies.
    pub(crate) context_count: usize,
    /// Per context and value parameter, whether the declaration owns a default for it. A
    /// declaration that publishes none admits no omission.
    pub(crate) defaults: Option<&'a [bool]>,
    /// The operands the export declares, inserted receiver included.
    pub(crate) declared: usize,
}

impl DependencyExportCall {
    /// The export operand positions a call leaving out the parameters `omitted` (checked call
    /// ordinals) leaves to the declaration's default entry.
    /// Ordinals keep the declaration's context prefix. A source extension receiver is inserted
    /// after it, and a serialized extension receiver occupies the ordinal after it; neither a
    /// context nor a receiver is ever omitted. Each ordinal names one distinct value parameter
    /// whose declaration owns a default, or the call is malformed.
    pub(crate) fn omitted_operands(
        self,
        omitted: &[u32],
        roles: DeclaredOperandRoles<'_>,
    ) -> Result<Vec<usize>, &'static str> {
        if omitted.is_empty() {
            return Ok(Vec::new());
        }
        let DeclaredOperandRoles {
            context_count,
            defaults,
            declared,
        } = roles;
        // Where the receiver sits among the export operands, and whether call ordinals count it.
        let (receiver, ordinal_counts_receiver) = match self {
            Self::TopLevel => (None, false),
            Self::SerializedExtension => (Some(context_count), true),
            Self::SourceExtension {
                receiver_position, ..
            } if receiver_position == context_count => (Some(context_count), false),
            Self::SourceExtension { .. } => {
                return Err("a dependency extension receiver outside its context prefix")
            }
            _ => {
                return Err(
                    "a dependency call with omitted arguments through a receiver-erased export",
                )
            }
        };
        let defaults = defaults.ok_or(
            "a dependency call omitting a parameter of a declaration without published defaults",
        )?;
        if defaults.len() + usize::from(receiver.is_some()) != declared {
            return Err(
                "a dependency declaration whose default flags are not parallel to its export",
            );
        }
        let mut positions = Vec::with_capacity(omitted.len());
        for &ordinal in omitted {
            let ordinal = ordinal as usize;
            if ordinal < context_count {
                return Err("a dependency call omitting a context operand");
            }
            let (parameter, position) = match receiver {
                None => (ordinal, ordinal),
                Some(receiver) if ordinal_counts_receiver => {
                    if ordinal == receiver {
                        return Err("a dependency call omitting its extension receiver");
                    }
                    (ordinal - 1, ordinal)
                }
                Some(_) => (ordinal, ordinal + 1),
            };
            match defaults.get(parameter) {
                Some(true) => positions.push(position),
                Some(false) => {
                    return Err("a dependency call omitting a parameter without a declared default")
                }
                None => {
                    return Err("a dependency call omitting an operand its export does not declare")
                }
            }
        }
        positions.sort_unstable();
        if positions.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("a dependency call omitting one operand twice");
        }
        Ok(positions)
    }
}

pub(crate) struct DependencyCallShape<'a> {
    pub(crate) is_top_level_function: bool,
    pub(crate) is_extension: bool,
    pub(crate) source_receiver: Option<Ty>,
    pub(crate) context_count: usize,
    /// The instantiated selected declaration, not the stored physical export signature.
    pub(crate) emitted_params: &'a [Ty],
    pub(crate) call_params: &'a [Ty],
    pub(crate) extension_receiver_parameter: Option<u32>,
    pub(crate) has_dispatch_receiver: bool,
    pub(crate) unread_receiver: Option<Ty>,
    pub(crate) companion_receiver_position: Option<usize>,
    pub(crate) member_receiver: Option<Ty>,
    pub(crate) receiver_is_exact_singleton: bool,
}

impl DependencyCallShape<'_> {
    pub(crate) fn authenticate(self) -> Result<DependencyExportCall, &'static str> {
        let Self {
            is_top_level_function,
            is_extension,
            source_receiver,
            context_count,
            emitted_params,
            call_params,
            extension_receiver_parameter,
            has_dispatch_receiver,
            unread_receiver,
            companion_receiver_position,
            member_receiver,
            receiver_is_exact_singleton,
        } = self;
        match (unread_receiver, companion_receiver_position) {
            (Some(receiver), Some(receiver_position)) => {
                if !is_top_level_function
                    || is_extension
                    || has_dispatch_receiver
                    || receiver_position != context_count
                {
                    return Err(
                        "a classifier-associated dependency receiver whose declaration disagrees",
                    );
                }
                return match extension_receiver_parameter {
                    None if receiver_position <= call_params.len()
                        && emitted_params == call_params =>
                    {
                        Ok(DependencyExportCall::ClassifierAssociated {
                            receiver,
                            receiver_position,
                        })
                    }
                    Some(serialized_position)
                        if usize::try_from(serialized_position).ok() == Some(receiver_position)
                            && receiver_position < call_params.len()
                            && call_params[receiver_position] == receiver
                            && call_params.len() == emitted_params.len() + 1
                            && call_params[..receiver_position]
                                == emitted_params[..receiver_position]
                            && call_params[receiver_position + 1..]
                                == emitted_params[receiver_position..] =>
                    {
                        Ok(DependencyExportCall::SerializedClassifierAssociated { receiver })
                    }
                    _ => Err(
                        "a classifier-associated dependency receiver whose declaration disagrees",
                    ),
                };
            }
            (Some(_), None) if !is_extension => {
                return Err(
                    "a classifier-associated dependency receiver without matching body authority",
                );
            }
            (None, Some(_)) => {
                return Err(
                    "a classifier-associated dependency receiver without matching body authority",
                );
            }
            (Some(_), None) | (None, None) => {}
        }
        if !is_extension && extension_receiver_parameter.is_none() && emitted_params != call_params
        {
            return Err("an emitted dependency callable whose checked parameter types changed");
        }
        match (
            is_top_level_function,
            is_extension,
            extension_receiver_parameter,
            has_dispatch_receiver,
        ) {
            (true, false, None, false) => Ok(DependencyExportCall::TopLevel),
            (false, false, None, true) => match (member_receiver, receiver_is_exact_singleton) {
                (None, true) => Ok(DependencyExportCall::SingletonMember),
                (Some(_), false) => Ok(DependencyExportCall::Member),
                _ => Err("an emitted dependency callable whose checked receiver shape changed"),
            },
            (false, false, Some(receiver_position), true) => {
                let receiver_position = usize::try_from(receiver_position).map_err(|_| {
                    "a dependency member extension with an invalid receiver coordinate"
                })?;
                if member_receiver.is_none()
                    || receiver_position != context_count
                    || emitted_params != call_params
                    || source_receiver.is_none()
                    || emitted_params.get(receiver_position) != source_receiver.as_ref()
                {
                    return Err("a dependency member extension whose receiver contract disagrees with its call");
                }
                Ok(DependencyExportCall::MemberExtension)
            }
            (false, true, None, true) => {
                let receiver = source_receiver
                    .ok_or("a dependency extension without its recorded receiver type")?;
                let receiver_position = context_count;
                if receiver_position > call_params.len()
                    || emitted_params.len() != call_params.len() + 1
                    || emitted_params[..receiver_position] != call_params[..receiver_position]
                    || emitted_params[receiver_position] != receiver
                    || emitted_params[receiver_position + 1..] != call_params[receiver_position..]
                {
                    return Err(
                        "a dependency extension whose receiver coordinate disagrees with its call",
                    );
                }
                if receiver_is_exact_singleton && unread_receiver == Some(receiver) {
                    Ok(DependencyExportCall::SourceSingletonExtension {
                        receiver,
                        receiver_position,
                    })
                } else {
                    Ok(DependencyExportCall::SourceExtension {
                        receiver,
                        receiver_position,
                    })
                }
            }
            (false, true, Some(receiver_position), false) => {
                let receiver_position = usize::try_from(receiver_position)
                    .map_err(|_| "a dependency extension with an invalid receiver coordinate")?;
                if receiver_position != context_count
                    || emitted_params != call_params
                    || source_receiver.is_none()
                    || emitted_params.get(receiver_position) != source_receiver.as_ref()
                {
                    return Err("a serialized dependency extension whose receiver coordinate disagrees with its declaration");
                }
                Ok(DependencyExportCall::SerializedExtension)
            }
            _ => Err("an emitted dependency callable whose checked receiver shape changed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DeclaredOperandRoles, DependencyExportCall};
    use crate::types::Ty;

    fn roles(context_count: usize, defaults: &[bool], declared: usize) -> DeclaredOperandRoles<'_> {
        DeclaredOperandRoles {
            context_count,
            defaults: Some(defaults),
            declared,
        }
    }

    #[test]
    fn omitted_operands_keep_contexts_and_follow_a_receiver() {
        let all = [true, true, true];
        assert_eq!(
            DependencyExportCall::TopLevel.omitted_operands(&[2, 0], roles(0, &all, 3)),
            Ok(vec![0, 2])
        );
        let context = [false, true, true];
        assert_eq!(
            DependencyExportCall::TopLevel.omitted_operands(&[2], roles(1, &context, 3)),
            Ok(vec![2])
        );
        let extension = DependencyExportCall::SourceExtension {
            receiver: Ty::String,
            receiver_position: 0,
        };
        assert_eq!(
            extension.omitted_operands(&[0, 1], roles(0, &[true, true], 3)),
            Ok(vec![1, 2])
        );
        // A context, the inserted receiver, then the omitted value parameter.
        let contextual = DependencyExportCall::SourceExtension {
            receiver: Ty::String,
            receiver_position: 1,
        };
        assert_eq!(
            contextual.omitted_operands(&[1], roles(1, &[false, true], 3)),
            Ok(vec![2])
        );
        // The serialized receiver is the call ordinal after the contexts.
        assert_eq!(
            DependencyExportCall::SerializedExtension
                .omitted_operands(&[2], roles(1, &[false, true], 3)),
            Ok(vec![2])
        );
        assert_eq!(
            DependencyExportCall::TopLevel.omitted_operands(&[], roles(0, &[], 0)),
            Ok(Vec::new())
        );
    }

    #[test]
    fn a_malformed_omission_is_rejected_before_any_operand_is_built() {
        let all = [true, true, true];
        let top = DependencyExportCall::TopLevel;
        let contextual = DependencyExportCall::SourceExtension {
            receiver: Ty::String,
            receiver_position: 1,
        };
        let serialized = DependencyExportCall::SerializedExtension;
        for (call, omitted, roles, error) in [
            (
                top,
                &[1, 1][..],
                roles(0, &all, 3),
                "a dependency call omitting one operand twice",
            ),
            (
                top,
                &[2, 0, 2][..],
                roles(0, &all, 3),
                "a dependency call omitting one operand twice",
            ),
            (
                top,
                &[3][..],
                roles(0, &all, 3),
                "a dependency call omitting an operand its export does not declare",
            ),
            (
                top,
                &[0][..],
                roles(1, &all, 3),
                "a dependency call omitting a context operand",
            ),
            (
                top,
                &[1][..],
                roles(0, &[true, false, true], 3),
                "a dependency call omitting a parameter without a declared default",
            ),
            (
                top,
                &[0][..],
                roles(0, &[true], 3),
                "a dependency declaration whose default flags are not parallel to its export",
            ),
            (
                contextual,
                &[0][..],
                roles(1, &[true, true], 3),
                "a dependency call omitting a context operand",
            ),
            (
                contextual,
                &[1, 1][..],
                roles(1, &[false, true], 3),
                "a dependency call omitting one operand twice",
            ),
            (
                contextual,
                &[0][..],
                roles(0, &[true, true], 3),
                "a dependency extension receiver outside its context prefix",
            ),
            (
                serialized,
                &[1][..],
                roles(1, &[false, true], 3),
                "a dependency call omitting its extension receiver",
            ),
            (
                serialized,
                &[0][..],
                roles(1, &[true, true], 3),
                "a dependency call omitting a context operand",
            ),
            (
                DependencyExportCall::Member,
                &[0][..],
                roles(0, &[true], 2),
                "a dependency call with omitted arguments through a receiver-erased export",
            ),
        ] {
            assert_eq!(
                call.omitted_operands(omitted, roles),
                Err(error),
                "{omitted:?}"
            );
        }
        assert_eq!(
            top.omitted_operands(
                &[0],
                DeclaredOperandRoles {
                    context_count: 0,
                    defaults: None,
                    declared: 1,
                },
            ),
            Err("a dependency call omitting a parameter of a declaration without published defaults")
        );
    }
}
