//! Library value classes this target carries as their underlying scalar.
//!
//! A dependency body, once expanded, constructs such a class from its underlying value and reads
//! that value back through the class's storage property (`UInt(this.data + other.data)`). Where
//! the class and its declared underlying type share one scalar carrier, an instance IS that value:
//! construction evaluates to its argument and the storage read to its receiver. The provider's
//! value-class declaration says what the class wraps and which property stores it; `carrier` is
//! this target's representation decision. Neither is read from the class's name.

use crate::types::{DeclaredValueClass, TypeName};

use super::*;

#[cfg(test)]
mod tests;

/// The scalar a value class `classifier` declared as `declaration` travels as, when this target
/// carries the class and its underlying type alike; `None` when the class is carried otherwise
/// (as an object reference) or its declared underlying type uses a different carrier.
fn scalar_representation(
    classifier: TypeName,
    declaration: &DeclaredValueClass,
) -> Result<Option<ValType>, Unsupported> {
    let carried = carrier(Ty::obj_name(classifier).canonical_semantic())?;
    let underlying = carrier(declaration.underlying)?;
    Ok(match carried {
        Some(scalar @ (ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64))
            if underlying == carried =>
        {
            Some(scalar)
        }
        _ => None,
    })
}

impl Body<'_> {
    /// The provider's declaration of `classifier` when it is a value class this target carries as
    /// its underlying scalar.
    fn scalar_value_class(
        &self,
        classifier: TypeName,
    ) -> Result<Option<DeclaredValueClass>, Unsupported> {
        let Some(declaration) = self
            .classifiers
            .classifier(classifier)
            .and_then(|fact| fact.value_declaration.clone())
        else {
            return Ok(None);
        };
        Ok(scalar_representation(classifier, &declaration)?.map(|_| declaration))
    }

    /// `V(value)` for a library value class `internal` carried as its underlying scalar: the
    /// value itself. `None` when `internal` is not such a class.
    pub(super) fn scalar_value_construction(
        &mut self,
        internal: TypeName,
        args: &[u32],
        selected: Option<&[Ty]>,
    ) -> Option<Result<(), Unsupported>> {
        let declaration = match self.scalar_value_class(internal) {
            Ok(declaration) => declaration?,
            Err(error) => return Some(Err(error)),
        };
        let underlying = [declaration.underlying];
        Some(match args {
            [argument] if selected.is_none_or(|selected| selected == underlying) => {
                self.value(*argument, declaration.underlying)
            }
            _ => Err(format!(
                // Boundary conversion for a diagnostic.
                "a construction of the value class `{}` other than from its underlying value",
                internal.render().replace('/', ".")
            )),
        })
    }

    /// Whether `target` is the storage property of a library value class this target carries as
    /// its underlying scalar, as that class's declaration names it.
    pub(super) fn is_scalar_value_storage(&self, target: crate::fir::ExternalPropertyId) -> bool {
        self.callables.property(target).is_some_and(|property| {
            self.scalar_value_class(property.owner)
                .ok()
                .flatten()
                .is_some_and(|declaration| {
                    declaration.property == property.name
                        && property.result == declaration.underlying
                })
        })
    }

    /// `receiver.storage` for a scalar-carried value class: the receiver's own value.
    pub(super) fn scalar_value_storage_read(
        &mut self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Result<(), Unsupported> {
        let property = self
            .callables
            .property(target)
            .ok_or("a value-class storage read without its property")?;
        let class = Ty::obj_name(property.owner).canonical_semantic();
        let underlying = property.result;
        self.value(receiver, class)?;
        self.coerce_emitted_value(class, underlying)
    }
}
