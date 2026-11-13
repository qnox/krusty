//! Enum classes: what stays target-specific once the shared enum lowering has run.
//!
//! [`krusty::backend::enums`] builds each enum of this file as ordinary common IR: a static slot per
//! constant, one initializer per enum, and `values()`/`valueOf` as functions. What it leaves here
//! is representation. `kotlin.Enum` is a base no file declares; its `name` and `ordinal` are the
//! two fields of `$Enum` ([`super::super::super::objects::ObjectRoot::enum_base`]) that every enum
//! class's struct extends. A constant is allocated with them filled before any constructor runs,
//! and their reads load them. `valueOf`'s failure throws the runtime `IllegalArgumentException`.

use krusty::backend::enums::BaseMember;
use krusty::ir::ClassId;
use krusty::libraries::RuntimeExceptionKind;

use super::*;
use crate::objects::ENUM_NAME_FIELD;

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
        if !self
            .classes
            .is_some_and(|classes| classes.is_enum_rooted(class))
        {
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
                BaseMember::Name => ENUM_NAME_FIELD,
                BaseMember::Ordinal => ENUM_NAME_FIELD + 1,
            },
        );
        Ok(())
    }

    /// `valueOf` found no constant of that name: Kotlin throws `IllegalArgumentException`, which
    /// kotlinc-wasm words `No enum constant <the enum's Kotlin name>.<name>`.
    pub(super) fn no_such_enum_constant(
        &mut self,
        classifier: krusty::types::TypeName,
        name: u32,
    ) -> Result<(), Unsupported> {
        let structure = self.runtime.exception_class(
            self.module,
            self.classifiers,
            RuntimeExceptionKind::IllegalArgument.classifier(),
        )?;
        let prefix = format!(
            "No enum constant {}.",
            Ty::obj_name(classifier).source_name()
        )
        .encode_utf16()
        .collect::<Vec<_>>();
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
        )?;
        self.code.throw(self.runtime.exceptions.tag);
        Ok(())
    }
}
