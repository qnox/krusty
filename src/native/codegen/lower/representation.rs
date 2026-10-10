//! Semantic-to-physical value representation at the Native lowering boundary.

use super::*;
use crate::types::TypeName;

impl BodyLowering<'_, '_, '_> {
    /// An expression coerced to the carrier of `target`.
    pub(super) fn coerce(&mut self, arg: u32, target: Ty) -> Result<Option<Value>, Unsupported> {
        let Some(value) = self.expression(arg)? else {
            // `Unit` is a value in Kotlin, and a position that wants a reference wants that value:
            // `val u: Any = Unit`, an argument of type `Any?`, a `Unit`-returning lambda's result.
            // The runtime owns the singleton, so there is one of it program-wide.
            if !self.terminated && self.carrier(target) == Carrier::Ref {
                return self.runtime_call("kt_unit", &[], any(), &[]);
            }
            return Ok(None);
        };
        if self.terminated {
            return Ok(Some(value));
        }
        let source = self.type_of(arg);
        self.convert(value, source, target)
    }

    /// An expression in a position that requires a reference, boxing a scalar if necessary.
    pub(super) fn reference(&mut self, id: u32) -> Result<Value, Unsupported> {
        match self.coerce(id, any())? {
            Some(value) => Ok(value),
            // Only a lowering that already left has no value here; anything else, including
            // `Unit`, `coerce` materialized.
            None => Ok(self.builder.ins().iconst(types::I64, 0)),
        }
    }

    /// What a construction of `class` answers, given the object its constructor filled: that
    /// object, except for a value class, whose instance is the VALUE. The constructor ran on a box
    /// — its `init` blocks and property initializers are the class's own, and that is where they
    /// run — and the value is read back out of it.
    pub(super) fn constructed(
        &mut self,
        object: Value,
        class: ClassId,
    ) -> Result<Option<Value>, Unsupported> {
        if self.terminated {
            return Ok(Some(object));
        }
        let classifier = self.file.ir.classes[class as usize].fq_name;
        if !self.file.values.is_value_class(classifier) {
            return Ok(Some(object));
        }
        self.unbox_value(object, classifier).map(Some)
    }

    /// A value of value class `classifier` put in a box of its own type — what `Any`, a type
    /// parameter or an interface holds it as. Only the value is stored: Kotlin's box runs no
    /// constructor, the value having been validated when it was made.
    pub(super) fn box_value(
        &mut self,
        value: Value,
        classifier: TypeName,
    ) -> Result<Value, Unsupported> {
        let class = self.value_class(classifier)?;
        let (offset, _) = self.file.value_storage(class)?;
        let descriptor = self.file.classes[class as usize].descriptor;
        let size = self.file.model.layout(class).instance_size;
        let object = self.allocate(descriptor, size)?;
        self.builder.ins().store(trusted(), value, object, offset);
        Ok(object)
    }

    /// The value a box of value class `classifier` holds.
    pub(super) fn unbox_value(
        &mut self,
        object: Value,
        classifier: TypeName,
    ) -> Result<Value, Unsupported> {
        let class = self.value_class(classifier)?;
        let (offset, ty) = self.file.value_storage(class)?;
        let clif = self.carrier(ty).clif().expect("a value is never `Unit`");
        Ok(self.builder.ins().load(clif, trusted(), object, offset))
    }

    /// The class of this file a boxed value class is laid out by. A value class declared
    /// somewhere else has no layout here, and boxing one declines.
    fn value_class(&self, classifier: TypeName) -> Result<ClassId, Unsupported> {
        self.file.ir.class_id_by_name(classifier).ok_or_else(|| {
            declined!(
                "a boxed `{}`, a value class this file does not declare",
                classifier.render().replace('/', ".")
            )
        })
    }

    /// A representation change between two SEMANTIC types.
    ///
    /// A value class travels as its value wherever the representation policy projects it and as
    /// a box of its own type everywhere else, so crossing between the two is a boxing or an
    /// unboxing of THAT class — never of the value inside, whose box would answer `is`,
    /// `toString` and `equals` as the wrong type. Everything else is a change between the
    /// projected types' carriers.
    pub(super) fn convert(
        &mut self,
        value: Value,
        source: Option<Ty>,
        target: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let values = self.file.values;
        let unboxed_source = source.and_then(|source| values.unboxed(source));
        let unboxed_target = values.unboxed(target);
        match (unboxed_source, unboxed_target) {
            // One class on both sides, unboxed on both — `V` into `V?` where `V?` is carried as
            // the value: the change, if any, is between the values' own carriers.
            (Some(from), Some(to)) if from == to => self.convert_carriers(
                value,
                source.map(|source| values.project(source)),
                values.project(target),
            ),
            (Some(from), _) => {
                let source = source.expect("an unboxed source has a type");
                let boxed = self.box_nullable(value, from, source)?;
                self.convert_carriers(boxed, Some(any()), values.project(target))
            }
            (None, Some(to)) => self.unbox_nullable(value, to, target),
            (None, None) => self.convert_carriers(
                value,
                source.map(|source| values.project(source)),
                values.project(target),
            ),
        }
    }

    /// Box a value of value class `class` carried unboxed at `ty`. A `V?` carried as its value
    /// uses the value's own `null` for the outer one, and that `null` stays `null`.
    fn box_nullable(
        &mut self,
        value: Value,
        class: TypeName,
        ty: Ty,
    ) -> Result<Value, Unsupported> {
        if !ty.is_nullable() {
            return self.box_value(value, class);
        }
        let is_null = self.is_null(value);
        let present = self.builder.create_block();
        let merge = self.builder.create_block();
        self.builder.append_block_param(merge, types::I64);
        self.builder
            .ins()
            .brif(is_null, merge, &[BlockArg::Value(value)], present, &[]);
        self.continue_in(present);
        self.builder.seal_block(present);
        let boxed = self.box_value(value, class)?;
        self.builder.ins().jump(merge, &[BlockArg::Value(boxed)]);
        self.continue_in(merge);
        self.builder.seal_block(merge);
        Ok(self.builder.block_params(merge)[0])
    }

    /// The value in a box of value class `class`, where `target` carries it unboxed. A `null`
    /// reaching a `V?` carried as the value is that value's own `null`.
    fn unbox_nullable(
        &mut self,
        object: Value,
        class: TypeName,
        target: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        if !target.is_nullable() {
            return self.unbox_value(object, class).map(Some);
        }
        let is_null = self.is_null(object);
        let present = self.builder.create_block();
        let merge = self.builder.create_block();
        self.builder.append_block_param(merge, types::I64);
        self.builder
            .ins()
            .brif(is_null, merge, &[BlockArg::Value(object)], present, &[]);
        self.continue_in(present);
        self.builder.seal_block(present);
        let value = self.unbox_value(object, class)?;
        self.builder.ins().jump(merge, &[BlockArg::Value(value)]);
        self.continue_in(merge);
        self.builder.seal_block(merge);
        Ok(Some(self.builder.block_params(merge)[0]))
    }

    /// A representation change between carriers of types already PROJECTED: boxing a scalar into
    /// a reference, unboxing one out, or widening/narrowing between scalars. An undetermined
    /// source leaves the value alone.
    pub(super) fn convert_carriers(
        &mut self,
        value: Value,
        source: Option<Ty>,
        target: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let target_ty = target;
        let target = self.carrier(target);
        match (source.map(|ty| self.carrier(ty)), target) {
            (None, target) => {
                // The lowering could not type the expression. Its machine type is still known,
                // and when that already is the target's carrier nothing needs doing; otherwise a
                // conversion would be a guess, and a guess is declined.
                let actual = self.builder.func.dfg.value_type(value);
                match target.clif() {
                    Some(clif) if clif == actual => Ok(Some(value)),
                    None => Ok(None),
                    Some(clif) => Err(declined!(
                        "a value of undetermined type (carried as `{actual}`) where a `{clif}` is \
                         required"
                    )),
                }
            }
            (Some(Carrier::Ref), Carrier::Ref) => Ok(Some(value)),
            (Some(source), target) if source == target => Ok(Some(value)),
            (Some(Carrier::Scalar(_, _)), Carrier::Ref) => {
                let ty = source.expect("known scalar");
                let Some(suffix) = box_suffix(ty) else {
                    return Err(declined!(
                        "a `{ty:?}` in a position that requires a reference"
                    ));
                };
                self.runtime_call(
                    &format!("kt_box_{suffix}"),
                    &[ty],
                    Ty::obj("kotlin/Any"),
                    &[value],
                )
            }
            (Some(Carrier::Ref), Carrier::Scalar(_, _)) => {
                // The TARGET's own type, not one recovered from its carrier: a carrier cannot tell
                // a `UByte` from a `Boolean`, and each unboxes through its own descriptor.
                let ty = target_ty.non_null();
                let Some(suffix) = box_suffix(ty) else {
                    return Err(declined!("an unboxing to `{ty:?}`"));
                };
                self.runtime_call(
                    &format!("kt_unbox_{suffix}"),
                    &[Ty::obj("kotlin/Any")],
                    ty,
                    &[value],
                )
            }
            (Some(Carrier::Scalar(from, signed)), Carrier::Scalar(to, _)) => {
                Ok(Some(self.resize(value, from, signed, to)))
            }
            (Some(_), Carrier::Void) => Ok(None),
            (Some(Carrier::Void), _) => Err("a coercion from `Unit`".into()),
        }
    }
}
