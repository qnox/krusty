//! The parameter list and result a callable carries in common IR, derived once from its header.
//!
//! Common lowering builds a function's IR signature from these, and a backend that numbers the
//! dispatch slots of a class declared in another file reads the same answer from the header, so
//! the two can never disagree about a member's representation.

use crate::types::Ty;

use super::{CallableId, DeclarationFlags, ResolvedModuleIndex};

impl ResolvedModuleIndex {
    /// The parameters (context parameters, then the extension receiver, then the declared value
    /// parameters) and result of `callable` as common IR declares its function.
    ///
    /// A compiler-generated member stores its values, so `Unit` crosses as the `kotlin.Unit`
    /// singleton. A `companion { … }` member's extension receiver is a name-resolution coordinate,
    /// never a value, and is left out. `None` when the callable has no published header or its
    /// receiver position is out of range.
    pub(crate) fn callable_carriers(&self, callable: CallableId) -> Option<(Vec<Ty>, Ty)> {
        let header = self.callable(callable)?;
        let flags = self.declaration_header(header.declaration)?.flags;
        let signature = self.signature(header.declaration)?;
        let compiler_generated = flags.has(DeclarationFlags::COMPILER_GENERATED);
        let carried = |ty: Ty| {
            if compiler_generated {
                crate::types::stored_value_ty(ty)
            } else {
                ty
            }
        };
        let mut parameters = signature
            .parameters
            .iter()
            .map(|parameter| carried(parameter.get()))
            .collect::<Vec<_>>();
        if !flags.has(DeclarationFlags::COMPANION) {
            if let Some(receiver) = header.shape.extension_receiver {
                let position = header.shape.context_parameter_count as usize;
                if position > parameters.len() {
                    return None;
                }
                parameters.insert(position, receiver.get());
            }
        }
        Some((parameters, carried(signature.result.get())))
    }
}
