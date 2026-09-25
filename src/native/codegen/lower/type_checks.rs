//! The type questions every lowering asks: which runtime descriptor a type names, `is` and `as`,
//! the null check, and the objects the RUNTIME constructs — the throwables it provides and its
//! `StringBuilder`.
//!
//! None of these needs a class of this file. A type written at a check or a cast names a
//! descriptor, and the ones the runtime defines — the value types' boxes, `String`, the throwable
//! hierarchy — are there whether or not the program declares anything. A class of the program
//! answers the same questions through the descriptor its own emission defines; see
//! `super::objects`.

use super::*;
use crate::types::TypeName;

/// What an `is` against a type Kotlin settles by itself answers.
#[derive(Clone, Copy)]
enum Settled {
    /// True of every value.
    Always,
    /// True of no value.
    Never,
    /// True of `null` only.
    Null,
    /// True of everything but `null`.
    NotNull,
}

/// Which of Kotlin's four `Throwable` constructors a site wrote, with the operands it gave.
///
/// The two one-argument forms are told apart by the parameter TYPE — a `Throwable` is a cause and
/// anything else is a message — and they are not one case with an empty field, because
/// `Throwable(cause)` fills the MESSAGE from the cause as well.
pub(super) enum ThrowableOperands {
    /// `Throwable()` and `Throwable(message)`; the no-argument form carries a `null`.
    Message(Value),
    /// `Throwable(cause)`, whose message the runtime renders from the cause.
    Cause(Value),
    /// `Throwable(message, cause)`.
    MessageAndCause { message: Value, cause: Value },
}

/// Whether the runtime, not this file, constructs `internal`: a `Throwable` it provides, or its
/// `StringBuilder`.
pub(super) fn is_runtime_constructed(internal: TypeName) -> bool {
    super::super::super::intrinsics::throwable_descriptor(internal).is_some()
        || super::super::super::intrinsics::is_string_builder(internal)
}

/// A type's spelling for a diagnostic: the class name when it has one, else the debug form.
pub(super) fn type_name_of(ty: Ty) -> String {
    match ty.non_null().obj_internal() {
        Some(internal) => internal.render().replace('/', "."),
        None => format!("{ty:?}"),
    }
}

impl<'a> FileLowering<'a> {
    /// A descriptor the runtime defines, declared as an import on first use.
    pub(super) fn import_data(&mut self, symbol: &str) -> Result<DataId, Unsupported> {
        if let Some(id) = self.data_imports.get(symbol) {
            return Ok(*id);
        }
        let id = self
            .module
            .declare_data(symbol, Linkage::Import, false, false)
            .map_err(|error| format!("importing `{symbol}` ({error})"))?;
        self.data_imports.insert(symbol.to_string(), id);
        Ok(id)
    }

    /// The runtime descriptor for a type an `is`/`as` names: one the runtime declares.
    pub(super) fn type_descriptor(&mut self, ty: Ty) -> Result<Option<DataId>, Unsupported> {
        let target = ty.non_null();
        let symbol = match target {
            Ty::String => "kt_type_string",
            Ty::Boolean => "kt_type_boolean",
            Ty::Byte => "kt_type_byte",
            Ty::Short => "kt_type_short",
            Ty::Int => "kt_type_int",
            Ty::Long => "kt_type_long",
            Ty::Char => "kt_type_char",
            Ty::Float => "kt_type_float",
            Ty::Double => "kt_type_double",
            Ty::Unit => "kt_type_unit",
            // The four unsigned types. Each is a descriptor of its own precisely so that an `is`
            // against it can answer, and so that `1u as? Int` cannot.
            Ty::UByte => "kt_type_ubyte",
            Ty::UShort => "kt_type_ushort",
            Ty::UInt => "kt_type_uint",
            Ty::ULong => "kt_type_ulong",
            // Kotlin's two built-in supertypes of the value types. Neither has instances of its
            // own, so each is a descriptor the boxes point at — which is why naming one here is
            // enough for `is` and needs nothing at the site.
            _ if target
                .obj_internal()
                .is_some_and(|name| name.matches("kotlin/Number")) =>
            {
                "kt_type_number"
            }
            _ if target
                .obj_internal()
                .is_some_and(|name| name.matches("kotlin/Comparable")) =>
            {
                "kt_type_comparable"
            }
            // `CharSequence` is a third of the same kind: no instances of its own, and both the
            // string and the builder point at it.
            _ if target
                .obj_internal()
                .is_some_and(super::super::super::intrinsics::is_char_sequence) =>
            {
                "kt_type_char_sequence"
            }
            // `Unit` reaches a type check spelled as the object it is rather than as the carrier
            // `Ty::Unit` names, and it is one type either way.
            _ if target
                .obj_internal()
                .is_some_and(|name| name.matches("kotlin/Unit")) =>
            {
                "kt_type_unit"
            }
            // The `Throwable` hierarchy. Like the built-in supertypes above these are the
            // RUNTIME's classes, declared in no file, so there is no class id to find one by —
            // and a `catch` clause is a type check against exactly these, which is what made the
            // omission visible: everything else that asks a type question here asks it of a class
            // the program wrote or of a value type.
            _ if target
                .obj_internal()
                .and_then(super::super::super::intrinsics::throwable_descriptor)
                .is_some() =>
            {
                target
                    .obj_internal()
                    .and_then(super::super::super::intrinsics::throwable_descriptor)
                    .expect("just matched")
            }
            _ => return Ok(None),
        };
        self.import_data(symbol).map(Some)
    }
}

impl<'a, 'b, 'c> BodyLowering<'a, 'b, 'c> {
    /// The receiver of a member access, evaluated to a reference.
    pub(super) fn receiver(&mut self, receiver: u32) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(receiver)?;
        Ok((!self.terminated).then_some(value))
    }

    /// The descriptor a CAST to `ty` must be checked against, or `None` when the cast is a
    /// representation change rather than a question about an object.
    ///
    /// Anything the target holds as a REFERENCE and the runtime names is a question about an
    /// object, so `as` and `as?` ask it exactly as `is` does — a class of this file, an array, a
    /// `String`. A SCALAR target is excluded although
    /// [`FileLowering::type_descriptor`] names one for it: `x as Int` is an unboxing, whose whole
    /// realization is the coercion the caller falls through to, and routing it through `kt_cast`
    /// would hand back the box where the site expects the number.
    fn checked_cast_target(&mut self, ty: Ty) -> Result<Option<DataId>, Unsupported> {
        // A cast to a type PARAMETER is a cast to its bound, which is the only thing left of it at
        // run time and exactly what kotlinc checks: `fun <T : CharSequence> f(x: Any?) = x as T`
        // rejects a non-`CharSequence` inside `f`, before the call site's own cast to the argument
        // it was given. An unbounded parameter bounds at `Any?`, where there is nothing to check.
        let ty = match ty.non_null() {
            Ty::TyParam(_, bound) => *bound,
            _ => ty,
        };
        let target = ty.non_null();
        if carrier(target) != Carrier::Ref {
            return Ok(None);
        }
        self.file.type_descriptor(target)
    }

    /// The `is` checks Kotlin settles by the type alone — `Nothing`, which has no instances, and
    /// `Any`, which every non-`null` value is one of — and `None` when the question really is
    /// about the object.
    fn settled_by_language(type_operand: Ty) -> Option<Settled> {
        let target = type_operand.non_null();
        let nullable = type_operand.is_nullable();
        let any = target
            .obj_internal()
            .is_some_and(super::super::super::intrinsics::is_any);
        let settled = match (target, any) {
            // `x is Nothing` is false; `x is Nothing?` admits only `null`.
            (Ty::Nothing, _) => Settled::Null,
            // `x is Any` is "not null"; `x is Any?` is true of everything.
            (_, true) if nullable => Settled::Always,
            (_, true) => Settled::NotNull,
            _ => return None,
        };
        Some(match (settled, nullable) {
            (Settled::Null, false) => Settled::Never,
            (other, _) => other,
        })
    }

    /// A check [`Self::settled_by_language`] answered. The receiver is evaluated either way: the
    /// constant is the answer, not the expression, and a receiver may have effects.
    fn language_instance_check(
        &mut self,
        op: IrTypeOp,
        arg: u32,
        settled: Settled,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(object) = self.receiver(arg)? else {
            return Ok(None);
        };
        let mut answer = match settled {
            Settled::Always => self.builder.ins().iconst(types::I8, 1),
            Settled::Never => self.builder.ins().iconst(types::I8, 0),
            Settled::Null => self.is_null(object),
            Settled::NotNull => {
                let is_null = self.is_null(object);
                let one = self.builder.ins().iconst(types::I8, 1);
                self.builder.ins().bxor(is_null, one)
            }
        };
        if op == IrTypeOp::NotInstanceOf {
            let one = self.builder.ins().iconst(types::I8, 1);
            answer = self.builder.ins().bxor(answer, one);
        }
        Ok(Some(answer))
    }

    /// `is`, `as`, `as?` and the coercions the frontend inserts.
    pub(super) fn type_operation(
        &mut self,
        op: IrTypeOp,
        arg: u32,
        type_operand: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        match op {
            IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => {
                // Two of Kotlin's types answer without asking the object anything, because their
                // membership is settled by the language rather than by a descriptor. `Nothing` has
                // no instances at all, and everything that is not `null` is an `Any`. The
                // nullability of the operand carries the rest: `is Nothing?` is exactly `== null`,
                // and `is Any?` is true of everything. The receiver is still evaluated — the
                // constant is the answer, not the expression.
                if let Some(settled) = Self::settled_by_language(type_operand) {
                    return self.language_instance_check(op, arg, settled);
                }
                let Some(descriptor) = self.file.type_descriptor(type_operand)? else {
                    return Err(format!(
                        "an `is` check against `{}`",
                        type_name_of(type_operand)
                    ));
                };
                // A SCALAR operand is asked the same way, through its box. Kotlin has no subtyping
                // among the primitive types, so the answer is settled statically — but `5 is
                // Number` and `1u is Comparable<*>` are true, and each primitive's box carries the
                // descriptor that says so (an unsigned one its own, which is what makes `1u is
                // Int` false). Boxing and asking is therefore both correct and the only rule
                // needed, where a static answer would need the hierarchy this generator lacks.
                let Some(object) = self.receiver(arg)? else {
                    return Ok(None);
                };
                let descriptor = self.data_address(descriptor);
                let mut result = self
                    .runtime_call(
                        "kt_is_instance",
                        &[any(), any()],
                        Ty::Boolean,
                        &[object, descriptor],
                    )?
                    .expect("`kt_is_instance` returns a Boolean");
                if type_operand.is_nullable() {
                    // `x is T?` admits `null`; `kt_is_instance(null, …)` is false, so `or` it in.
                    let is_null = self.is_null(object);
                    result = self.builder.ins().bor(result, is_null);
                }
                if op == IrTypeOp::NotInstanceOf {
                    let one = self.builder.ins().iconst(types::I8, 1);
                    result = self.builder.ins().bxor(result, one);
                }
                Ok(Some(result))
            }
            // A cast to something WEARING a descriptor is a question the runtime answers; a cast
            // to anything else is a representation change, and the coercion is its whole
            // realization.
            IrTypeOp::Cast | IrTypeOp::CastNonNull => {
                // A cast whose TARGET is a primitive but whose SOURCE is a reference is still a
                // question about the object: `(1 as Any) as Byte` is a `ClassCastException` in
                // Kotlin, because a boxed `Int` is not a `Byte` — the widths are convertible and
                // the TYPES are not. Unboxing first and converting would answer `1` to a program
                // Kotlin refuses, so the descriptor is asked before anything is read out. It is
                // the same `kt_cast`, against the value type's own descriptor, which is exactly
                // what those descriptors exist for.
                let target = type_operand.non_null();
                // A cast whose TARGET is a primitive is still a question about the object whenever
                // the source is not already that same primitive. Two sources reach here:
                //
                //  - a reference: `(1 as Any) as Byte`, plainly an object question;
                //  - a DIFFERENT primitive: `it as Byte` where `it` is an erased type parameter
                //    the call substituted to `Int`. Kotlin has no cast between two primitive types
                //    — `val x: Int = 1; x as Byte` does not compile — so a scalar-to-other-scalar
                //    cast can only have come from erasure, and the question it is really asking is
                //    the one the value's own box would answer.
                //
                // Unboxing first and converting would answer `1` to a program Kotlin refuses with
                // a ClassCastException, so the descriptor is asked before anything is read out.
                let source_carrier = self.type_of(arg).map(carrier);
                if carrier(target) != Carrier::Ref
                    && source_carrier.is_none_or(|source| source != carrier(target))
                {
                    let Some(descriptor) = self.file.type_descriptor(target)? else {
                        return self.coerce(arg, type_operand);
                    };
                    let Some(object) = self.receiver(arg)? else {
                        return Ok(None);
                    };
                    let descriptor = self.data_address(descriptor);
                    let helper = if type_operand.is_nullable() {
                        "kt_cast"
                    } else {
                        "kt_cast_non_null"
                    };
                    let checked =
                        self.runtime_call(helper, &[any(), any()], any(), &[object, descriptor])?;
                    let Some(checked) = checked else {
                        return Ok(None);
                    };
                    if self.terminated {
                        return Ok(None);
                    }
                    return self.convert(checked, Some(Ty::nullable(target)), type_operand);
                }
                let Some(descriptor) = self.checked_cast_target(type_operand)? else {
                    // No descriptor to test against: an erased type PARAMETER, or a classifier
                    // this file does not declare. The type is gone — but whether the cast was a
                    // NON-NULL one is not, and Kotlin still checks that much: `null as T` where
                    // `T : Any` raises a NullPointerException, and so does `t as (T & Any)`.
                    // Without it the null travels on to a caller that unboxes it, which is a fault
                    // rather than an answer.
                    //
                    // The node's OWN operation is what decides, never the spelling of the target:
                    // an unbounded `T` is not nullable as a `Ty` and `null as T` is still legal,
                    // because `T` may be instantiated with a nullable type. Reading the target
                    // instead made eight programs throw that Kotlin accepts.
                    //
                    // The exception carries no message where a cast to a NAMED type gives one:
                    // there is no name left to put in it. That is what erasure costs.
                    let coerced = self.coerce(arg, type_operand)?;
                    if self.terminated {
                        return Ok(None);
                    }
                    let Some(value) = coerced else {
                        return Ok(None);
                    };
                    if op != IrTypeOp::CastNonNull
                        || type_operand.is_nullable()
                        || carrier(type_operand) != Carrier::Ref
                    {
                        return Ok(Some(value));
                    }
                    return self.runtime_call("kt_not_null", &[any()], any(), &[value]);
                };
                let helper = if op == IrTypeOp::Cast || type_operand.is_nullable() {
                    "kt_cast"
                } else {
                    "kt_cast_non_null"
                };
                let Some(object) = self.receiver(arg)? else {
                    return Ok(None);
                };
                let descriptor = self.data_address(descriptor);
                self.runtime_call(helper, &[any(), any()], any(), &[object, descriptor])
            }
            IrTypeOp::SafeCast => {
                let Some(descriptor) = self.checked_cast_target(type_operand)? else {
                    return Err(format!("an `as?` to `{}`", type_name_of(type_operand)));
                };
                let Some(object) = self.receiver(arg)? else {
                    return Ok(None);
                };
                let descriptor = self.data_address(descriptor);
                self.runtime_call(
                    "kt_safe_cast",
                    &[any(), any()],
                    any(),
                    &[object, descriptor],
                )
            }
            IrTypeOp::ImplicitCoercion => {
                let arg = self
                    .erased_nullable_carrier(arg, type_operand)
                    .unwrap_or(arg);
                self.coerce(arg, type_operand)
            }
        }
    }

    /// The erased reference beneath `Object -> P -> P?`, when that is what `arg` is.
    ///
    /// A generic declaration returns through an erased reference, and lowering records reading it
    /// as a specialized primitive `P` and then widening that to `P?` as two coercions: `fun <T>
    /// f(): T = null as T` stored into an `Int?`. Realized one after the other they unbox a
    /// reference that may be `null` only to box it again, and a `null` fails the unbox. The pair
    /// is one reference conversion, so the widening takes the reference directly. This is the
    /// same fold the JVM backend makes at its result boundary.
    fn erased_nullable_carrier(&self, arg: u32, target: Ty) -> Option<u32> {
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: inner,
            type_operand,
        } = self.file.ir.expr(arg)
        else {
            return None;
        };
        let primitive = target.nullable_primitive()?;
        (*type_operand == primitive
            && carrier(primitive) != Carrier::Ref
            && self
                .type_of(*inner)
                .is_some_and(|ty| carrier(ty) == Carrier::Ref))
        .then_some(*inner)
    }

    /// Construction of a type the RUNTIME owns: declared in no file, and allocated and filled by
    /// the runtime rather than laid out here. [`is_runtime_constructed`] says which types those are.
    pub(super) fn runtime_construction(
        &mut self,
        internal: TypeName,
        args: &[u32],
        selected: Option<&[Ty]>,
    ) -> Result<Option<Value>, Unsupported> {
        let name = internal.render();
        // A `Throwable` the RUNTIME provides. These classes are declared in no file either, but they do carry state — the
        // message — so the runtime allocates and fills one rather than the generator doing it.
        if let Some(descriptor) = super::super::super::intrinsics::throwable_descriptor(internal) {
            return self.runtime_throwable(descriptor, &name, args, selected);
        }
        // `StringBuilder()`, the runtime's growable text buffer. Declared in no file either, for
        // the same reason the list above is not. The capacity overload is a HINT; `StringBuilder(s)`
        // COPIES the text, because a builder is about to be written through and the string it was
        // handed is a value.
        if super::super::super::intrinsics::is_string_builder(internal) {
            return match (args, selected) {
                ([], _) => self.runtime_call("kt_string_builder_new", &[], any(), &[]),
                ([argument], Some([only])) if only.non_null() == Ty::Int => {
                    let Some(capacity) = self.coerce(*argument, Ty::Int)? else {
                        return Ok(None);
                    };
                    if self.terminated {
                        return Ok(None);
                    }
                    self.runtime_call(
                        "kt_string_builder_with_capacity",
                        &[Ty::Int],
                        any(),
                        &[capacity],
                    )
                }
                ([argument], _) => {
                    let text = self.reference(*argument)?;
                    if self.terminated {
                        return Ok(None);
                    }
                    self.runtime_call("kt_string_builder_with_text", &[any()], any(), &[text])
                }
                _ => Err(format!("this constructor of `{name}`")),
            };
        }
        Err(format!("construction of `{name}`"))
    }

    /// The operands a `Throwable` constructor was given, as its two fields take them.
    ///
    /// Kotlin declares four: `()`, `(message)`, `(cause)` and `(message, cause)`. The two
    /// one-argument forms are told apart by the parameter TYPE — a `Throwable` is a cause and
    /// anything else is a message — and they differ in more than which field they fill, because
    /// `Throwable(cause)` takes its MESSAGE from the cause as well. That rendering belongs to the
    /// runtime, which is where `toString` lives, so this answers which form was written and lets
    /// the caller pick the entry point. `Ok(None)` means the lowering left.
    pub(super) fn throwable_operands(
        &mut self,
        name: &str,
        args: &[u32],
        selected: Option<&[Ty]>,
    ) -> Result<Option<ThrowableOperands>, Unsupported> {
        let cause_operand = |ty: &Ty| {
            ty.non_null()
                .obj_internal()
                .and_then(super::super::super::intrinsics::throwable_descriptor)
                .is_some()
        };
        match (args, selected) {
            // `Throwable(cause)`: the runtime fills the message from it.
            ([argument], Some([only])) if cause_operand(only) => {
                let Some(cause) =
                    self.coerce(*argument, Ty::nullable(Ty::obj("kotlin/Throwable")))?
                else {
                    return Err(format!("a `Unit` cause for `{name}`"));
                };
                if self.terminated {
                    return Ok(None);
                }
                Ok(Some(ThrowableOperands::Cause(cause)))
            }
            // `Throwable(message, cause)`.
            ([message, cause], Some([first, second])) if cause_operand(second) => {
                let Some(message) =
                    self.throwable_message_operand(name, &[*message], Some(&[*first]))?
                else {
                    return Ok(None);
                };
                let Some(cause) = self.coerce(*cause, Ty::nullable(Ty::obj("kotlin/Throwable")))?
                else {
                    return Err(format!("a `Unit` cause for `{name}`"));
                };
                if self.terminated {
                    return Ok(None);
                }
                Ok(Some(ThrowableOperands::MessageAndCause { message, cause }))
            }
            _ => {
                let Some(message) = self.throwable_message_operand(name, args, selected)? else {
                    return Ok(None);
                };
                Ok(Some(ThrowableOperands::Message(message)))
            }
        }
    }

    /// The message operand a `Throwable` constructor was given, as its `message` field takes it: a
    /// `null` reference where the constructor took none.
    ///
    /// Only the no-argument and one-message forms reach here; the forms carrying a cause are told
    /// apart by [`Self::throwable_operands`] before this is asked. `Ok(None)` means the lowering
    /// left.
    fn throwable_message_operand(
        &mut self,
        name: &str,
        args: &[u32],
        selected: Option<&[Ty]>,
    ) -> Result<Option<Value>, Unsupported> {
        use super::super::super::intrinsics::ThrowableMessage;
        let message = match (args, selected) {
            ([], _) => None,
            ([argument], Some([only])) => {
                match super::super::super::intrinsics::throwable_message(only) {
                    Some(kind) => Some((*argument, kind)),
                    None => return Err(format!("this constructor of `{name}`")),
                }
            }
            _ => return Err(format!("this constructor of `{name}`")),
        };
        let Some((argument, kind)) = message else {
            // Kotlin's `null` message, which `toString` reports as the type name alone.
            return Ok(Some(self.builder.ins().iconst(types::I64, 0)));
        };
        // A `Rendered` message is its `toString`, which is what the runtime's own renderer
        // answers — so a scalar overload (`AssertionError(42)`) crosses as the box that renderer
        // takes, and a reference goes straight to it.
        let value = match kind {
            ThrowableMessage::Verbatim => self.expression(argument)?,
            ThrowableMessage::Rendered => {
                let value = self.coerce(argument, Ty::nullable(Ty::obj("kotlin/Any")))?;
                if self.terminated {
                    return Ok(None);
                }
                let Some(value) = value else {
                    return Err(format!("a `Unit` message for `{name}`"));
                };
                self.runtime_call("kt_to_string", &[any()], any(), &[value])?
            }
        };
        if self.terminated {
            return Ok(None);
        }
        match value {
            Some(value) => Ok(Some(value)),
            None => Err(format!("a `Unit` message for `{name}`")),
        }
    }

    /// `Throwable(…)` and its subclasses, as the runtime declares them — all four of Kotlin's
    /// constructors, each reaching the runtime entry point that fills what it was given.
    fn runtime_throwable(
        &mut self,
        descriptor: &str,
        name: &str,
        args: &[u32],
        selected: Option<&[Ty]>,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(operands) = self.throwable_operands(name, args, selected)? else {
            return Ok(None);
        };
        if self.terminated {
            return Ok(None);
        }
        let descriptor = self.file.import_data(descriptor)?;
        let descriptor = self.data_address(descriptor);
        let (symbol, mut arguments) = match operands {
            ThrowableOperands::Message(message) => ("kt_throwable_new", vec![message]),
            ThrowableOperands::Cause(cause) => ("kt_throwable_new_from_cause", vec![cause]),
            ThrowableOperands::MessageAndCause { message, cause } => {
                ("kt_throwable_new_with_cause", vec![message, cause])
            }
        };
        arguments.insert(0, descriptor);
        let signature = vec![any(); arguments.len()];
        let thrown = self.runtime_call(symbol, &signature, any(), &arguments)?;
        Ok(Some(thrown.expect("a throwable constructor answers one")))
    }

    /// The runtime reader for `e.message` or `e.cause`, and `None` for any other accessor.
    pub(super) fn throwable_field(
        &self,
        target: crate::fir::ExternalPropertyId,
    ) -> Option<&'static str> {
        let property = self.file.provider.external_property(target)?;
        let getter = self.file.provider.external_callable(property.getter)?;
        super::super::super::intrinsics::throwable_field(getter.callable.owner, &property.name)
    }

    /// `e.message` and `e.cause` — the two fields a `Throwable` carries, read by the runtime
    /// rather than by an offset here, because the class is the runtime's and so is its layout.
    pub(super) fn throwable_field_read(
        &mut self,
        symbol: &str,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(symbol, &[any()], any(), &[value])
    }
}
