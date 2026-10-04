//! The Kotlin-level signatures kotlinc gives the representation methods of a value class (the ones it
//! generates, and the user-written members and accessors this pass moves onto the carrier while
//! synthesizing them), which their generic `Signature` attributes spell.
//!
//! kotlinc builds each of them as an IR function before it maps any type to the JVM
//! (`MemoizedInlineClassReplacements`): `constructor-impl` copies the constructor, so it declares
//! the class's type parameters and returns the class type `C<T>`; a static replacement takes the
//! moved dispatch receiver typed `C<T>`; and `equals-impl0` compares two `C<*>`. Their descriptors
//! erase those positions to the carrier, and the signature writer expands each of them the same way
//! it expands any value-class position. They are recorded here, at synthesis, because the function's
//! own parameter list gains the carrier here and the erasure that follows replaces its types.

use crate::ir::{IrFile, IrGenericSig};
use crate::types::Ty;

/// What one generated representation method declares.
pub(super) enum RepresentationMember<'a> {
    /// `constructor-impl` over the constructor's declared parameters.
    Constructor(&'a [Ty]),
    /// A static replacement over the moved receiver, then the member's own parameters `rest`.
    Receiver { rest: &'a [Ty], ret: Ty },
    /// `equals-impl0(C<*>, C<*>): Boolean`.
    SpecializedEquals,
}

pub(super) fn record(ir: &mut IrFile, class_id: u32, function: u32, member: RepresentationMember) {
    let class = &ir.classes[class_id as usize];
    let class_type = ir.class_type(class);
    let type_params = ir
        .class_signature_name(class.fq_name)
        .map(|signature| signature.type_params.clone())
        .unwrap_or_default();
    let (type_params, params, ret) = match member {
        RepresentationMember::Constructor(params) => (type_params, params.to_vec(), class_type),
        RepresentationMember::Receiver { rest, ret } => (
            // A moved member keeps only its own type parameters.
            ir.signatures
                .get(&function)
                .map(|signature| signature.type_params.clone())
                .unwrap_or_default(),
            std::iter::once(class_type)
                .chain(rest.iter().copied())
                .collect(),
            ret,
        ),
        RepresentationMember::SpecializedEquals => {
            let arguments = type_params
                .iter()
                .map(|parameter| Ty::star_projection(parameter.upper_bound()))
                .collect::<Vec<_>>();
            let star = Ty::obj_args_name(class.fq_name, &arguments);
            (Vec::new(), vec![star, star], Ty::Boolean)
        }
    };
    ir.jvm_value_class_member_signatures.insert(
        function,
        IrGenericSig {
            type_params,
            params,
            ret: Some(ret),
            supers: Vec::new(),
        },
    );
}
