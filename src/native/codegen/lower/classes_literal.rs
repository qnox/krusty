//! `x::class` and `String::class` — a `kotlin.reflect.KClass`.
//!
//! Both forms answer one type descriptor, and which descriptor is the whole of the difference: a
//! literal over a TYPE names it statically, while a literal over a VALUE reads the descriptor the
//! object is wearing. `val x: CharSequence = ""` answers `String::class` for that reason, and a
//! scalar receiver is boxed first so there is a descriptor to read at all — which is also why
//! `(n++)::class` answers `Int::class` and not the class of some primitive that has no object.
//!
//! The object is an ordinary allocation rather than a canonical instance, because Kotlin's `KClass`
//! is equal by the class it stands for and not by identity. The runtime answers `equals` on the
//! descriptor, so `x::class == String::class` is true without a table of canonical instances
//! existing anywhere.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// `::class`, over a value or over a type.
    pub(super) fn class_literal(
        &mut self,
        classifier: Option<Ty>,
        value: Option<u32>,
    ) -> Result<Option<Value>, Unsupported> {
        // A bound literal carries its static type too, and the RUNTIME one wins: that is the whole
        // reason Kotlin lets a program write `x::class` rather than naming the type.
        if let Some(value) = value {
            let object = self.reference(value)?;
            if self.terminated {
                return Ok(None);
            }
            return self.runtime_call("kt_class_of", &[any()], kclass(), &[object]);
        }
        let Some(ty) = classifier else {
            return Err("a class literal naming neither a type nor a value".to_string());
        };
        let Some(descriptor) = self.file.type_descriptor(ty)? else {
            return Err(format!("a class literal of `{ty:?}`"));
        };
        let address = self.data_address(descriptor);
        self.runtime_call("kt_class_literal", &[Ty::Long], kclass(), &[address])
    }

    /// `k.simpleName` and `k.qualifiedName`, or `None` when the read is somebody else's.
    ///
    /// Both are read off the descriptor's own Kotlin name, which every type carries because
    /// `toString` already needed it — so no reflection metadata has to exist for either.
    pub(super) fn class_name_accessor(
        &self,
        target: crate::fir::ExternalPropertyId,
    ) -> Option<&'static str> {
        let property = self.file.provider.external_property(target)?;
        let getter = self.file.provider.external_callable(property.getter)?;
        super::super::super::intrinsics::class_name_accessor(getter.callable.owner, &property.name)
    }

    pub(super) fn class_name(
        &mut self,
        symbol: &str,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(symbol, &[any()], Ty::nullable(Ty::String), &[object])
    }
}

/// The type a class literal answers with.
pub(super) fn kclass() -> Ty {
    Ty::obj("kotlin/reflect/KClass")
}
