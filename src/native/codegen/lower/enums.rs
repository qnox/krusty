//! Enum classes: what stays target-specific once the shared enum lowering has run.
//!
//! [`crate::backend::enums`] builds each enum of this file as ordinary common IR: a static slot per
//! constant, one initializer per enum, and `values()`/`valueOf` as functions. What it leaves here
//! is representation. `kotlin.Enum` is a base no file declares, whose storage of `name` and
//! `ordinal` the runtime owns (`model::ENUM_NAME_OFFSET`, `model::ENUM_ORDINAL_OFFSET`): a constant
//! is constructed with that storage filled before any constructor runs, its two reads load it, the
//! `entries` list is the runtime's, and so is the failure of `valueOf`.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// One enum constant: its construction, with `kotlin.Enum`'s storage filled right after the
    /// allocation, so a constructor reading `name` or `ordinal` finds them.
    pub(super) fn enum_constant(
        &mut self,
        name: &str,
        ordinal: u32,
        construction: u32,
    ) -> Result<Option<Value>, Unsupported> {
        self.enum_base = Some((name.into(), ordinal));
        let constant = self.expression(construction)?;
        if self.enum_base.take().is_some() {
            return Err(declined!(
                "an enum constant built without its own allocation (`{name}`)"
            ));
        }
        Ok(constant)
    }

    /// Fill a freshly allocated constant's `kotlin.Enum` storage, when the construction being
    /// lowered is one.
    pub(super) fn store_enum_base(
        &mut self,
        base: Option<(Box<str>, u32)>,
        object: Value,
    ) -> Result<(), Unsupported> {
        let Some((name, ordinal)) = base else {
            return Ok(());
        };
        let text = self.string_literal(name.as_bytes())?;
        self.builder
            .ins()
            .store(trusted(), text, object, model::ENUM_NAME_OFFSET as i32);
        let position = self.builder.ins().iconst(types::I32, i64::from(ordinal));
        self.builder.ins().store(
            trusted(),
            position,
            object,
            model::ENUM_ORDINAL_OFFSET as i32,
        );
        Ok(())
    }

    /// The enum's one `EnumEntries` list, over a fresh array of its constants.
    pub(super) fn enum_entries_list(&mut self, values: u32) -> Result<Option<Value>, Unsupported> {
        let values = self.reference(values)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_enum_entries_of", &[any()], any(), &[values])
    }

    /// `valueOf` found no constant of that name: Kotlin throws, and the runtime's failure is
    /// this one.
    pub(super) fn no_such_enum_constant(
        &mut self,
        name: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let text = self.reference(name)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_no_such_enum_constant", &[any()], Ty::Unit, &[text])?;
        self.builder.ins().trap(TrapCode::unwrap_user(1));
        self.terminate();
        Ok(None)
    }

    /// Whether an external property is one of `kotlin.Enum`'s two, and which.
    ///
    /// The property reaches here as an opaque identity; what it names is recovered through the
    /// getter frozen at the frontend/backend boundary. Emission consumes that selected identity;
    /// it never reopens the provider or looks the property up by spelling.
    pub(super) fn enum_member_name(
        &self,
        target: crate::fir::ExternalPropertyId,
    ) -> Option<&'static str> {
        let property = self.file.callables.property(target)?;
        let getter = self.file.callables.callable(property.getter)?;
        super::super::super::intrinsics::enum_member(getter.physical_owner, &property.name)
    }

    /// `name` and `ordinal`, which every enum constant answers from its own storage.
    pub(super) fn enum_member(
        &mut self,
        member: &str,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        self.null_check(object)?;
        Ok(Some(match member {
            "name" => self.builder.ins().load(
                types::I64,
                trusted(),
                object,
                model::ENUM_NAME_OFFSET as i32,
            ),
            _ => self.builder.ins().load(
                types::I32,
                trusted(),
                object,
                model::ENUM_ORDINAL_OFFSET as i32,
            ),
        }))
    }
}
