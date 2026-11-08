//! Enum classes: what stays target-specific once the shared enum lowering has run.
//!
//! [`crate::backend::enums`] builds each enum of this file as ordinary common IR: a static slot per
//! constant, one initializer per enum, and `values()`/`valueOf` as functions. What it leaves here
//! is representation. `kotlin.Enum` is a base no file declares, whose storage of `name` and
//! `ordinal` the runtime owns (`model::ENUM_NAME_OFFSET`, `model::ENUM_ORDINAL_OFFSET`): a constant
//! is allocated with that storage unset, as Kotlin/Native's `createUninitializedInstance` leaves
//! it, constructed in place with it filled before any constructor runs, its two reads load it, the
//! `entries` list is the runtime's, and so is the failure of `valueOf`.

use super::*;

/// The enum constant a construction builds: the `kotlin.Enum` storage it fills, and the static
/// slot already holding its allocation when it was published first.
pub(super) struct EnumBase {
    name: Box<str>,
    ordinal: u32,
    pub(super) published: Option<u32>,
}

impl BodyLowering<'_, '_, '_> {
    /// An enum constant whose declaring file already realized it. The call is that file's getter:
    /// the shared realization's initializer and the constant's own slot.
    pub(super) fn imported_enum_entry(
        &mut self,
        classifier: crate::types::TypeName,
        name: &str,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(symbol) = self
            .file
            .dependency_enum_entries
            .get(&(classifier, name.to_owned()))
        else {
            return Err(declined!(
                "an enum another file declares (`{}`)",
                classifier.render()
            ));
        };
        let id = self.file.import(symbol, &[], Ty::obj_name(classifier))?;
        let func_ref = self.func_ref(id);
        let call = self.emit_call(func_ref, &[])?;
        Ok(Some(self.builder.inst_results(call)[0]))
    }

    /// One enum constant: its construction, with `kotlin.Enum`'s storage filled right after the
    /// allocation, or on the published allocation it constructs in place, so a constructor reading
    /// `name` or `ordinal` finds them.
    pub(super) fn enum_constant(
        &mut self,
        name: &str,
        ordinal: u32,
        construction: u32,
        published: Option<u32>,
    ) -> Result<Option<Value>, Unsupported> {
        self.enum_base = Some(EnumBase {
            name: name.into(),
            ordinal,
            published,
        });
        let constant = self.expression(construction)?;
        if self.enum_base.take().is_some() {
            return Err(declined!(
                "an enum constant built without its own allocation (`{name}`)"
            ));
        }
        Ok(constant)
    }

    /// A constant allocated and nothing more: its header, every field zero, its `kotlin.Enum`
    /// storage unset and no constructor run.
    pub(super) fn enum_allocation(
        &mut self,
        instance: crate::types::TypeName,
    ) -> Result<Option<Value>, Unsupported> {
        let class = self.file.class_of(instance, "allocation of")?;
        let descriptor = self.file.classes[class as usize].descriptor;
        let size = self.file.model.layout(class).instance_size;
        Ok(Some(self.allocate(descriptor, size)?))
    }

    /// The published allocation a constant's construction initializes.
    pub(super) fn published_enum_constant(&mut self, slot: u32) -> Result<Value, Unsupported> {
        self.static_read(slot)?
            .ok_or_else(|| declined!("an enum constant's slot with no value"))
    }

    /// Fill a constant's `kotlin.Enum` storage, when the construction being lowered is one.
    pub(super) fn store_enum_base(
        &mut self,
        base: Option<EnumBase>,
        object: Value,
    ) -> Result<(), Unsupported> {
        let Some(EnumBase { name, ordinal, .. }) = base else {
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

    /// `name` and `ordinal`, which every enum constant answers from its own storage.
    pub(super) fn enum_member(
        &mut self,
        member: crate::backend::enums::BaseMember,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        self.null_check(object)?;
        Ok(Some(match member {
            crate::backend::enums::BaseMember::Name => self.builder.ins().load(
                types::I64,
                trusted(),
                object,
                model::ENUM_NAME_OFFSET as i32,
            ),
            crate::backend::enums::BaseMember::Ordinal => self.builder.ins().load(
                types::I32,
                trusted(),
                object,
                model::ENUM_ORDINAL_OFFSET as i32,
            ),
        }))
    }
}
