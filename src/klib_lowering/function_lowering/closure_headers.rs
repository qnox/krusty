//! Declaration-owned capture prefixes for lifted serialized closures.

use super::*;
use crate::ir::{
    IrCapturedDeclaration, IrCapturedReceiver, IrCapturingCallable, IrParameterRole, IrValueCapture,
};

#[cfg(test)]
mod tests;

pub(in super::super) struct CaptureParameter {
    pub(in super::super) symbol: KlibIrSymbol,
    pub(in super::super) ty: Ty,
    pub(in super::super) identity: IrParameterIdentity,
    pub(in super::super) receiver: Option<IrCapturedReceiver>,
}

impl FunctionHeader {
    pub(in super::super) fn capture_parameter(
        &self,
        symbol: &KlibIrSymbol,
        ordinal: u32,
        capturer: IrCapturingCallable,
        receivers: &[IrCapturedReceiver],
        is_closure: bool,
    ) -> Result<Option<CaptureParameter>, KlibBodyDeclineReason> {
        let Some(value) = self.value(symbol) else {
            return Ok(None);
        };
        let offset = usize::from(self.receiver_slot.is_some());
        let receiver = if (value.index as usize) < offset {
            Some(IrCapturedReceiver::Enclosing {
                classifier: self
                    .receiver_slot
                    .expect("a receiver slot has its classifier"),
            })
        } else {
            let original = &self.identities[value.index as usize - offset];
            match original.role {
                IrParameterRole::CapturedReceiver { ordinal } => Some(
                    receivers
                        .get(ordinal as usize)
                        .ok_or_else(|| {
                            mismatch("a captured receiver has no declaration fact".to_owned())
                        })?
                        .clone(),
                ),
                IrParameterRole::ExtensionReceiver => Some(if is_closure {
                    IrCapturedReceiver::Lambda(None)
                } else {
                    IrCapturedReceiver::Callable {
                        label: self.name.clone().into_boxed_str(),
                        owner: crate::fir::CapturedCallableOwner::Declaration,
                    }
                }),
                IrParameterRole::ContextReceiver { ordinal }
                | IrParameterRole::AnonymousContextParameter { ordinal } => {
                    let anonymous = matches!(
                        original.role,
                        IrParameterRole::AnonymousContextParameter { .. }
                    );
                    let types = self
                        .identities
                        .iter()
                        .zip(&self.params)
                        .filter_map(|(identity, ty)| {
                            (matches!(
                                identity.role,
                                IrParameterRole::AnonymousContextParameter { .. }
                            ) && anonymous
                                || matches!(identity.role, IrParameterRole::ContextReceiver { .. })
                                    && !anonymous)
                                .then_some(*ty)
                        })
                        .collect::<Vec<_>>()
                        .into_boxed_slice();
                    Some(IrCapturedReceiver::Context {
                        kind: if anonymous {
                            crate::types::CapturedContextKind::Anonymous
                        } else {
                            crate::types::CapturedContextKind::LegacyReceiver
                        },
                        types,
                        index: ordinal,
                    })
                }
                _ => None,
            }
        };
        let identity = match &receiver {
            Some(_) => IrParameterIdentity::captured_receiver(0),
            None => {
                let original = &self.identities[value.index as usize - offset];
                IrParameterIdentity::captured_value(
                    original.source_name.clone(),
                    ordinal,
                    IrValueCapture {
                        declaration: match original.role {
                            IrParameterRole::CapturedValue { capture, .. } => capture.declaration,
                            _ => IrCapturedDeclaration::Parameter,
                        },
                        capturer,
                    },
                )
            }
        };
        Ok(Some(CaptureParameter {
            symbol: symbol.clone(),
            ty: self.slot_type(value.index),
            identity,
            receiver,
        }))
    }
}

pub(in super::super) fn closure_header(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    enclosing: &FunctionHeader,
    captures: &[CaptureParameter],
) -> Result<FunctionHeader, KlibBodyDeclineReason> {
    if function.base.flags & crate::metadata::function_flags::IS_SUSPEND != 0 {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a suspend closure",
        ));
    }
    let mut header = raw_function_header_with_class(
        None,
        HeaderPurpose::Body(None),
        arena,
        function,
        enclosing.type_parameters.clone(),
    )?;
    let prefix = u32::try_from(captures.len()).expect("a capture list fits u32");
    for parameter in header.values.values_mut() {
        parameter.index += prefix;
    }
    for (index, capture) in captures.iter().enumerate() {
        if header
            .values
            .insert(
                capture.symbol.clone(),
                ParameterValue {
                    index: index as u32,
                    stable_read: true,
                    erased: None,
                },
            )
            .is_some()
        {
            return Err(mismatch(
                "a capture repeats an invocation parameter identity".to_owned(),
            ));
        }
    }
    header
        .params
        .splice(0..0, captures.iter().map(|capture| capture.ty));
    header.identities.splice(
        0..0,
        captures.iter().map(|capture| capture.identity.clone()),
    );
    header.defaults.splice(0..0, captures.iter().map(|_| None));
    // Prefix values are storage, not context or extension arguments of the lifted callable.
    // Invocation roles remain on their exact parameter identities after that prefix.
    header.context_count = 0;
    header.extension_receiver = false;
    header.singleton_extension_unread_receiver = None;
    Ok(header)
}
