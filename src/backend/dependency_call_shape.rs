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
