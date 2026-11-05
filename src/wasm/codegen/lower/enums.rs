//! Enum classes: what stays target-specific once the shared enum lowering has run.
//!
//! [`crate::backend::enums`] builds each enum of this file as ordinary common IR: a static slot per
//! constant, one initializer per enum, and `values()`/`valueOf` as functions. What it leaves here
//! is representation. `kotlin.Enum` is a base no file declares; its `name` and `ordinal` are the
//! two fields of `$Enum` ([`super::super::super::objects::ObjectRoot::enum_base`]) that every enum
//! class's struct extends. A constant is allocated with them filled before any constructor runs,
//! and their reads load them. `valueOf`'s failure throws the runtime `IllegalArgumentException`.

use crate::backend::enums::BaseMember;
use crate::ir::ClassId;
use crate::libraries::RuntimeExceptionKind;

use super::*;

impl Body<'_> {
    /// One enum constant: its construction, whose allocation fills `kotlin.Enum`'s fields so a
    /// constructor reading `name` or `ordinal` finds them.
    pub(super) fn enum_constant(
        &mut self,
        name: &str,
        ordinal: u32,
        construction: u32,
    ) -> Result<(), Unsupported> {
        self.enum_base = Some((name.into(), ordinal));
        self.expression(construction)?;
        if self.enum_base.take().is_some() {
            return Err(format!(
                "an enum constant built without its own allocation (`{name}`)"
            ));
        }
        Ok(())
    }

    /// Push the `$Enum` fields of an instance of `class` being allocated: the constant's own when
    /// this allocation builds one, otherwise their defaults, which the constant's allocation
    /// replaces.
    pub(super) fn enum_base_fields(&mut self, class: ClassId) -> Result<(), Unsupported> {
        if self.classes.map_or(0, |classes| classes.base_fields(class)) == 0 {
            return Ok(());
        }
        match self.enum_base.take() {
            Some((name, ordinal)) => {
                let units = name.encode_utf16().collect::<Vec<_>>();
                self.runtime
                    .string_constant(self.module, &mut self.code, &units);
                self.code.i32_const(ordinal as i32);
            }
            None => {
                self.code
                    .ref_null(HeapType::Concrete(self.runtime.string))
                    .i32_const(0);
            }
        }
        Ok(())
    }

    /// `name` or `ordinal`, read from the receiver's `$Enum` fields.
    pub(super) fn enum_member(
        &mut self,
        member: BaseMember,
        receiver: u32,
    ) -> Result<(), Unsupported> {
        let base = self.runtime.objects.enum_base;
        self.expression(receiver)?;
        self.code.ref_cast(false, HeapType::Concrete(base));
        self.code.struct_get(
            base,
            match member {
                BaseMember::Name => 1,
                BaseMember::Ordinal => 2,
            },
        );
        Ok(())
    }

    /// `valueOf` found no constant of that name: Kotlin throws `IllegalArgumentException`.
    pub(super) fn no_such_enum_constant(&mut self, name: u32) -> Result<(), Unsupported> {
        let structure = self.runtime.exceptions.ensure_class(
            self.module,
            self.classifiers,
            RuntimeExceptionKind::IllegalArgument.classifier(),
        )?;
        let prefix = "No enum constant ".encode_utf16().collect::<Vec<_>>();
        self.runtime
            .string_constant(self.module, &mut self.code, &prefix);
        self.string_of(name)?;
        self.code.call(self.runtime.string_concat);
        let message = self.local(STRING);
        self.code.local_set(message);
        self.runtime.exceptions.construct_class(
            &mut self.code,
            structure,
            Some(message),
            self.runtime.string,
        );
        self.code.throw(self.runtime.exceptions.tag);
        Ok(())
    }
}
