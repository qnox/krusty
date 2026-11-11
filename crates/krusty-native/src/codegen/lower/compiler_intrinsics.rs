//! Ordinary dependency-call forms of compiler-supplied operations.
//!
//! Source operators normally arrive as `IrIntrinsic`. A callable reference to the same selected
//! declaration, and every call of a target-stdlib intrinsic such as a bit reinterpretation, arrives
//! as an external call instead, retaining the provider's exact compiler-intrinsic fact. This
//! boundary consumes that identity; it never reconstructs one from an owner or callable spelling.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// The provider's bit reinterpretation of `operand`, a `from` scalar, as the equal-width `to`.
    fn reinterpret_bits(
        &mut self,
        operand: u32,
        from: Ty,
        to: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        if !matches!(
            (from, to),
            (Ty::Float, Ty::Int)
                | (Ty::Double, Ty::Long)
                | (Ty::Int, Ty::Float)
                | (Ty::Long, Ty::Double)
        ) {
            return Err(declined!(
                "a scalar reinterpretation from `{from:?}` to `{to:?}`"
            ));
        }
        let Some(value) = self.coerce(operand, from)? else {
            return Err("a scalar reinterpretation of `Unit`".into());
        };
        if self.terminated {
            return Ok(None);
        }
        let clif = self
            .carrier(to)
            .clif()
            .expect("a validated reinterpretation target has a scalar carrier");
        Ok(Some(self.bits_of(value, clif)))
    }

    fn primitive_unary(
        &mut self,
        operation: krusty::backend::BackendCompilerIntrinsic,
        operand: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        use krusty::backend::BackendCompilerIntrinsic;
        use krusty::libraries::PrimitiveUnaryIntrinsic;

        let Some(value) = self.coerce(operand, ret)? else {
            return Err("a primitive unary operator on `Unit`".into());
        };
        if self.terminated {
            return Ok(None);
        }
        let floating = matches!(ret.non_null(), Ty::Float | Ty::Double);
        Ok(Some(match operation {
            BackendCompilerIntrinsic::PrimitiveBitNot if !floating => {
                self.builder.ins().bnot(value)
            }
            BackendCompilerIntrinsic::PrimitiveUnary(PrimitiveUnaryIntrinsic::Identity) => value,
            BackendCompilerIntrinsic::PrimitiveUnary(PrimitiveUnaryIntrinsic::Negate) => {
                if floating {
                    self.builder.ins().fneg(value)
                } else {
                    self.builder.ins().ineg(value)
                }
            }
            _ => {
                return Err(declined!(
                    "the primitive operator `{operation:?}` on `{ret:?}`"
                ))
            }
        }))
    }

    /// A builtin scalar's own `hashCode()`, by the rule Kotlin fixes for the receiver's type.
    fn primitive_hash_code(&mut self, operand: u32) -> Result<Option<Value>, Unsupported> {
        let Some(ty) = self.checked_type(operand) else {
            return Err("a primitive `hashCode` without a checked receiver type".into());
        };
        if !matches!(
            ty,
            Ty::Boolean
                | Ty::Char
                | Ty::Byte
                | Ty::Short
                | Ty::Int
                | Ty::Long
                | Ty::Float
                | Ty::Double
        ) {
            return Err(declined!("a primitive `hashCode` on `{ty:?}`"));
        }
        let Some(value) = self.coerce(operand, ty)? else {
            return Err("a primitive `hashCode` on `Unit`".into());
        };
        if self.terminated {
            return Ok(None);
        }
        self.value_hash(value, ty).map(Some)
    }

    /// Realize an exact provider intrinsic reached through an ordinary member call.
    ///
    /// Returns `None` only when the selected declaration has no operation handled here, allowing
    /// the caller to continue through ordinary dependency realization.
    pub(super) fn compiler_intrinsic_call(
        &mut self,
        site: u32,
        intrinsic: Option<krusty::backend::BackendCompilerIntrinsic>,
        receiver: Option<u32>,
        args: &[u32],
        declared: DeclaredOperands<'_>,
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let extension_receiver_parameter = declared.extension_receiver_parameter;
        if let Some(plan) = intrinsic.and_then(super::super::dependency_intrinsics::operation_plan)
        {
            use super::super::dependency_intrinsics::DependencyOperation;
            return Some(match plan {
                DependencyOperation::ListStorage(plan) => {
                    if receiver.is_some()
                        || extension_receiver_parameter.is_some()
                        || !ret.is_reference()
                    {
                        return Some(Err(
                            "a list storage construction with malformed receiver or result".into(),
                        ));
                    }
                    self.list_storage_construction(plan, args)
                }
                DependencyOperation::StringPlus => {
                    let Some((left, right)) =
                        exact_extension_pair(receiver, args, extension_receiver_parameter)
                    else {
                        return Some(Err("a malformed `String.plus`".into()));
                    };
                    if ret != Ty::obj("kotlin/String") {
                        return Some(Err("a `String.plus` with a non-String result".into()));
                    }
                    self.string_plus(left, right, ret)
                }
                DependencyOperation::PrimitiveHashCode => {
                    let (Some(operand), [], None) = (receiver, args, extension_receiver_parameter)
                    else {
                        return Some(Err("a malformed primitive `hashCode`".into()));
                    };
                    if ret != Ty::Int {
                        return Some(Err("a primitive `hashCode` with a non-Int result".into()));
                    }
                    self.primitive_hash_code(operand)
                }
                // The iteration protocol's own member: the member path realizes it through the
                // receiver's runtime descriptor, exactly as it realizes a source call of it.
                DependencyOperation::PrimitiveIteratorNext => return None,
            });
        }
        if intrinsic == Some(krusty::backend::BackendCompilerIntrinsic::ReinterpretBits) {
            let operand = match (receiver, args) {
                (Some(receiver), []) => receiver,
                (None, [argument]) => *argument,
                _ => {
                    return Some(Err(
                        "an exact scalar reinterpretation with malformed operands".into(),
                    ))
                }
            };
            let from = match self.checked_type(operand) {
                Some(from) => from.non_null(),
                None => {
                    return Some(Err(
                        "an exact scalar reinterpretation without a checked operand type".into(),
                    ))
                }
            };
            return Some(self.reinterpret_bits(operand, from, ret));
        }
        // A primitive operator member — `a.and(b)` in an expanded library body — is the binary
        // operation its source operator form lowers to.
        if let Some(op) = intrinsic.and_then(IrBinOp::of_primitive_operator) {
            let (Some(lhs), [rhs]) = (receiver, args) else {
                return Some(Err("a primitive operator with malformed operands".into()));
            };
            return Some(self.binary(site, op, lhs, *rhs));
        }
        // `a.inv()`, `a.unaryMinus()` and `a.unaryPlus()` — the operators with no operand but
        // the receiver, at the result's carrier as the source operator forms are.
        if let Some(
            operation @ (krusty::backend::BackendCompilerIntrinsic::PrimitiveBitNot
            | krusty::backend::BackendCompilerIntrinsic::PrimitiveUnary(_)),
        ) = intrinsic
        {
            let (Some(operand), []) = (receiver, args) else {
                return Some(Err(
                    "a primitive unary operator with malformed operands".into()
                ));
            };
            return Some(self.primitive_unary(operation, operand, ret));
        }
        let floating_extension_operand = |operand: Ty| {
            let value = exact_extension_operand(receiver, args, extension_receiver_parameter)?;
            (self.checked_type(value).map(Ty::non_null) == Some(operand)).then_some(value)
        };
        match intrinsic {
            Some(krusty::backend::BackendCompilerIntrinsic::CollectionSizeOrDefault) => {
                let Some((iterable, fallback)) =
                    exact_extension_pair(receiver, args, extension_receiver_parameter)
                else {
                    return Some(Err(
                        "an exact collection sizing helper with malformed operands".into(),
                    ));
                };
                if ret != Ty::Int || self.checked_type(fallback) != Some(Ty::Int) {
                    return Some(Err(
                        "an exact collection sizing helper with a mismatched signature".into(),
                    ));
                }
                return Some(self.collection_size_or_default(iterable, fallback));
            }
            Some(krusty::backend::BackendCompilerIntrinsic::TextTrim) => {
                let Some(value) =
                    exact_extension_operand(receiver, args, extension_receiver_parameter)
                else {
                    return Some(Err("an exact text trim with malformed operands".into()));
                };
                return Some(self.text_trim(value, ret));
            }
            Some(krusty::backend::BackendCompilerIntrinsic::ClosedRangeContains) => {
                let operands = match (receiver, args) {
                    (Some(range), [value]) => (range, *value),
                    (None, [range, value]) if extension_receiver_parameter == Some(0) => {
                        (*range, *value)
                    }
                    _ => return Some(Err("a range membership with malformed operands".into())),
                };
                return Some(self.facade_range_contains(operands.0, operands.1));
            }
            Some(
                operation @ (krusty::backend::BackendCompilerIntrinsic::RangeUntil
                | krusty::backend::BackendCompilerIntrinsic::RangeDownTo),
            ) => {
                let Some((start, end)) =
                    exact_extension_pair(receiver, args, extension_receiver_parameter)
                else {
                    return Some(Err("an exact range builder with malformed operands".into()));
                };
                return Some(self.intrinsic_range_builder(operation, start, end, ret));
            }
            Some(
                operation @ (krusty::backend::BackendCompilerIntrinsic::UnsignedDivide { operand }
                | krusty::backend::BackendCompilerIntrinsic::UnsignedRemainder {
                    operand,
                }),
            ) => {
                let (None, [left, right]) = (receiver, args) else {
                    return Some(Err("an unsigned division with malformed operands".into()));
                };
                if ret != operand {
                    return Some(Err("an unsigned division with a mismatched result".into()));
                }
                let remainder = matches!(
                    operation,
                    krusty::backend::BackendCompilerIntrinsic::UnsignedRemainder { .. }
                );
                return Some(self.unsigned_division(operand, remainder, *left, *right));
            }
            Some(krusty::backend::BackendCompilerIntrinsic::ResultFailure(operation)) => {
                if receiver.is_some() {
                    return Some(Err("a Result helper with a dispatch receiver".into()));
                }
                return Some(self.result_operation(operation, args, ret));
            }
            Some(krusty::backend::BackendCompilerIntrinsic::RadixToString { operand }) => {
                let (None, [value, radix]) = (receiver, args) else {
                    return Some(Err("a radix rendering with malformed operands".into()));
                };
                let symbol = match operand {
                    Ty::Int => "kt_int_to_string_radix",
                    Ty::Long => "kt_long_to_string_radix",
                    _ => return Some(Err("a radix rendering of an unsupported width".into())),
                };
                if ret != Ty::obj("kotlin/String") {
                    return Some(Err("a radix rendering with a non-String result".into()));
                }
                let params = [operand, Ty::Int];
                return Some((|| {
                    let arguments = self.arguments(&[*value, *radix], &params)?;
                    if self.terminated {
                        return Ok(None);
                    }
                    self.runtime_call(symbol, &params, ret, &arguments)
                })());
            }
            Some(krusty::backend::BackendCompilerIntrinsic::FloatingIsNaN { operand }) => {
                let Some(value) = floating_extension_operand(operand) else {
                    return Some(Err(
                        "an exact floating-point `isNaN` with malformed operands".into(),
                    ));
                };
                if ret != Ty::Boolean {
                    return Some(Err(
                        "an exact floating-point `isNaN` with a non-Boolean result".into(),
                    ));
                }
                return Some(self.float_predicate(
                    super::super::super::intrinsics::FloatPredicate::IsNaN,
                    value,
                ));
            }
            Some(krusty::backend::BackendCompilerIntrinsic::FloatingIsInfinite { operand }) => {
                let Some(value) = floating_extension_operand(operand) else {
                    return Some(Err(
                        "an exact floating-point `isInfinite` with malformed operands".into(),
                    ));
                };
                if ret != Ty::Boolean {
                    return Some(Err(
                        "an exact floating-point `isInfinite` with a non-Boolean result".into(),
                    ));
                }
                return Some(self.float_predicate(
                    super::super::super::intrinsics::FloatPredicate::IsInfinite,
                    value,
                ));
            }
            _ => {}
        }
        let operation = match intrinsic {
            Some(krusty::backend::BackendCompilerIntrinsic::ArrayGet) => {
                Some(IrIntrinsic::ArrayGet)
            }
            Some(krusty::backend::BackendCompilerIntrinsic::ArraySet) => {
                Some(IrIntrinsic::ArraySet)
            }
            Some(krusty::backend::BackendCompilerIntrinsic::ArraySize) => {
                Some(IrIntrinsic::ArraySize)
            }
            Some(krusty::backend::BackendCompilerIntrinsic::StringGet) => {
                Some(IrIntrinsic::StringGet)
            }
            Some(krusty::backend::BackendCompilerIntrinsic::StringLength) => {
                Some(IrIntrinsic::StringLength)
            }
            Some(krusty::backend::BackendCompilerIntrinsic::NullableAnyToString) => {
                Some(IrIntrinsic::NullableAnyToString)
            }
            _ => None,
        };
        if let Some(operation) = operation {
            return Some(self.intrinsic(operation, ret, receiver, args));
        }
        match intrinsic {
            Some(krusty::backend::BackendCompilerIntrinsic::BooleanNot) => {
                let (Some(receiver), []) = (receiver, args) else {
                    return Some(Err("a malformed `Boolean.not`".into()));
                };
                if self.checked_type(receiver).map(Ty::non_null) != Some(Ty::Boolean) {
                    return Some(Err(
                        "a `Boolean.not` receiver with a non-Boolean type".into()
                    ));
                }
                Some(self.boolean_not(receiver, ret))
            }
            Some(krusty::backend::BackendCompilerIntrinsic::UnsignedCompare { carrier }) => {
                let (None, [left, right]) = (receiver, args) else {
                    return Some(Err("a malformed unsigned comparison".into()));
                };
                Some(self.unsigned_compare_intrinsic(carrier, *left, *right, ret))
            }
            // `a.compareTo(b)` on a builtin scalar, compared at the carrier the declaration's own
            // receiver and parameter promote to, as the source operator form is.
            Some(krusty::backend::BackendCompilerIntrinsic::PrimitiveCompare) => {
                let (Some(receiver), [argument], Some(owner), [parameter]) =
                    (receiver, args, declared.receiver, declared.params)
                else {
                    return Some(Err("a malformed primitive `compareTo`".into()));
                };
                let Some(operand) =
                    krusty::libraries::builtin_member_realization::primitive_compare_operand(
                        owner, *parameter,
                    )
                else {
                    return Some(Err(declined!(
                        "a primitive `compareTo` of `{owner:?}` with `{parameter:?}`"
                    )));
                };
                Some(self.primitive_compare(operand, receiver, *argument, ret))
            }
            _ => None,
        }
    }

    /// Both arguments are eager, including the fallback when Collection.size will be used.
    fn collection_size_or_default(
        &mut self,
        iterable: u32,
        fallback: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(iterable)?;
        if self.terminated {
            return Ok(None);
        }
        let Some(fallback) = self.coerce(fallback, Ty::Int)? else {
            return Err("a Unit fallback in collection sizing".into());
        };
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(
            "kt_collection_size_or_default",
            &[any(), Ty::Int],
            Ty::Int,
            &[value, fallback],
        )
    }

    /// `receiver.startCoroutine(completion)` for a `suspend () -> T` value that takes no arguments.
    ///
    /// The receiver runs first, then the completion. The suspend lambda's implementation takes
    /// that completion as its trailing argument and returns `Any?`; the result is then delivered
    /// to the completion. A lambda whose body actually suspends declines where that suspension is
    /// lowered. Another shape declines rather than calling the serialized body.
    pub(super) fn parameterless_start_coroutine(
        &mut self,
        site: u32,
        ret: Ty,
        dispatch_receiver: Option<u32>,
        args: &[u32],
        params: &[Ty],
    ) -> Result<Option<Value>, Unsupported> {
        let Some(receiver) = dispatch_receiver else {
            return Err("a startCoroutine of another shape".into());
        };
        if ret != Ty::Unit || params.len() != 1 || args.len() != 1 {
            return Err("a startCoroutine of another shape".into());
        }
        let Some(ty) = self
            .checked_type(receiver)
            .or_else(|| self.file.ir.ext_call_source_receiver.get(&site).copied())
        else {
            return Err("a startCoroutine without a function type".into());
        };
        let krusty::types::TyKind::Fun(signature) = ty.non_null().kind() else {
            return Err("a startCoroutine of another function".into());
        };
        if !signature.suspend
            || !signature.params.is_empty()
            || signature.context_count != 0
            || signature.has_receiver
        {
            return Err("a startCoroutine of another function".into());
        }
        if self.foreign_completion(args[0])? {
            return Err(declined!(
                "a startCoroutine completion that is not the factory continuation"
            ));
        }
        let Some(work) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let completion = self.reference(args[0])?;
        if self.terminated {
            return Ok(None);
        }
        let Some(result) = self.invoke_value(work, &[completion], any())? else {
            return Ok(None);
        };
        if self.terminated {
            return Ok(None);
        }
        self.deliver_completion(completion, result)?;
        Ok(None)
    }

    /// `Continuation(context, resumeWith)`. The context runs, then the completion function. The
    /// result is an object that retains both and forwards `resumeWith` to that function.
    pub(super) fn continuation_factory(
        &mut self,
        args: &[u32],
        params: &[Ty],
    ) -> Result<Option<Value>, Unsupported> {
        if params.len() != 2 || args.len() != 2 {
            return Err("a Continuation of another shape".into());
        }
        let context = self.continuation_operand(args[0])?;
        if self.terminated {
            return Ok(None);
        }
        let resume = self.continuation_operand(args[1])?;
        if self.terminated {
            return Ok(None);
        }
        self.continuation_object(context, resume)
    }

    /// Evaluate a factory operand through the ordinary expression path, so a dependency object is
    /// the instance its module exports.
    fn continuation_operand(&mut self, id: u32) -> Result<Value, Unsupported> {
        self.reference(id)
    }

    /// A completion whose value is known not to be the object this factory allocates.
    ///
    /// A cast does not change that. A call of the factory stays. A value whose static type is the
    /// factory's interface is not proof: it may be another object, and delivery checks the value
    /// before reading it. A value of a different type is refused here.
    fn foreign_completion(&self, id: u32) -> Result<bool, Unsupported> {
        let id = value_under_casts(self.file.ir, id);
        if factory_continuation(self.file, id) {
            return Ok(false);
        }
        if let IrExpr::New { internal, .. } = self.file.ir.expr(id) {
            return Ok(!self
                .file
                .callables
                .continuation_factory()
                .declares(*internal));
        }
        let Some(ty) = self.checked_type(id) else {
            return Ok(false);
        };
        Ok(ty
            .non_null()
            .obj_internal()
            .is_some_and(|name| !self.file.callables.continuation_factory().declares(name)))
    }

    /// An object whose first fields are the context and the `resumeWith` function.
    fn continuation_object(
        &mut self,
        context: Value,
        resume: Value,
    ) -> Result<Option<Value>, Unsupported> {
        let descriptor = self.file.continuation_descriptor()?;
        let object = self.allocate(descriptor, CONTINUATION_SIZE)?;
        self.builder
            .ins()
            .store(trusted(), context, object, CONTINUATION_CONTEXT);
        self.builder
            .ins()
            .store(trusted(), resume, object, CONTINUATION_RESUME);
        Ok(Some(object))
    }

    /// Invoke the `resumeWith` function stored on the factory's continuation.
    ///
    /// A value whose static type is the interface may still be another object. The header's type
    /// is within every object; only a match reads the continuation's fields or calls through the
    /// function stored there. A different object, including `null`, is left untouched.
    fn deliver_completion(&mut self, completion: Value, result: Value) -> Result<(), Unsupported> {
        let class = self.file.continuation_descriptor()?;
        let expected = self.data_address(class);
        let skip = self.builder.create_block();
        let check = self.builder.create_block();
        let deliver = self.builder.create_block();
        let is_null = self.is_null(completion);
        self.builder.ins().brif(is_null, skip, &[], check, &[]);

        self.continue_in(check);
        self.builder.seal_block(check);
        let header = self
            .builder
            .ins()
            .load(types::I64, trusted(), completion, 0);
        let same = self.builder.ins().icmp(IntCC::Equal, header, expected);
        self.builder.ins().brif(same, deliver, &[], skip, &[]);

        self.continue_in(deliver);
        self.builder.seal_block(deliver);
        let resume =
            self.builder
                .ins()
                .load(types::I64, trusted(), completion, CONTINUATION_RESUME);
        self.invoke_value(resume, &[result], Ty::Unit)?;
        if !self.terminated {
            self.builder.ins().jump(skip, &[]);
        }

        self.continue_in(skip);
        self.builder.seal_block(skip);
        Ok(())
    }

    /// `object`, checked to be the factory's continuation before one of its fields is read. A
    /// `context` or `resumeWith` call on another continuation fails with `ClassCastException`
    /// instead of reading that object's memory as those fields.
    fn checked_continuation(&mut self, object: Value) -> Result<Option<Value>, Unsupported> {
        let descriptor = self.file.continuation_descriptor()?;
        let descriptor = self.data_address(descriptor);
        let checked = self.runtime_call(
            "kt_cast_non_null",
            &[any(), any()],
            any(),
            &[object, descriptor],
        )?;
        Ok(checked.filter(|_| !self.terminated))
    }

    /// `receiver.context`: the context the factory stored.
    pub(super) fn continuation_context(
        &mut self,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        if self.terminated {
            return Ok(None);
        }
        let Some(object) = self.checked_continuation(object)? else {
            return Ok(None);
        };
        Ok(Some(self.builder.ins().load(
            types::I64,
            trusted(),
            object,
            CONTINUATION_CONTEXT,
        )))
    }

    /// `receiver.resumeWith(result)`: the function the factory stored, called with `result`.
    pub(super) fn continuation_resume(
        &mut self,
        receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let Some(receiver) = receiver else {
            return Err(declined!("a continuation resumeWith without a receiver"));
        };
        if args.len() != 1 {
            return Err(declined!("a continuation resumeWith of another shape"));
        }
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        if self.terminated {
            return Ok(None);
        }
        let argument = self.reference(args[0])?;
        if self.terminated {
            return Ok(None);
        }
        let Some(object) = self.checked_continuation(object)? else {
            return Ok(None);
        };
        let resume = self
            .builder
            .ins()
            .load(types::I64, trusted(), object, CONTINUATION_RESUME);
        self.invoke_value(resume, &[argument], Ty::Unit)?;
        Ok(None)
    }

    /// A read of the continuation interface's `context`, when `id` is that read.
    pub(super) fn continuation_property(
        &mut self,
        id: u32,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let owners = self.file.callables.continuation_factory();
        let (target, receiver) = {
            let IrExpr::Checked(operation) = self.file.ir.expr(id) else {
                return None;
            };
            let IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver,
                arguments,
                parameters,
                ..
            } = &**operation
            else {
                return None;
            };
            if !arguments.is_empty() || !parameters.is_empty() {
                return None;
            }
            (*target, *receiver)
        };
        let owner = self.file.callables.property(target)?.owner;
        if !owners.declares(owner) {
            return None;
        }
        let Some(receiver) = receiver else {
            return Some(Err(declined!("a continuation context without a receiver")));
        };
        Some(self.continuation_context(receiver))
    }
}

const CONTINUATION_CONTEXT: i32 = model::HEADER_SIZE as i32;
const CONTINUATION_RESUME: i32 = model::HEADER_SIZE as i32 + 8;
const CONTINUATION_SIZE: u32 = model::HEADER_SIZE + 16;

impl<'a> FileLowering<'a> {
    /// The descriptor of the continuation object, declared on first use.
    fn continuation_descriptor(&mut self) -> Result<DataId, Unsupported> {
        Ok(self.ensure_continuation()?.0)
    }

    /// The interface descriptor `is Continuation` asks about.
    pub(super) fn continuation_interface_descriptor(&mut self) -> Result<DataId, Unsupported> {
        Ok(self.ensure_continuation()?.1)
    }

    /// The class the factory allocates, and the interface that class implements.
    fn ensure_continuation(&mut self) -> Result<(DataId, DataId), Unsupported> {
        if let Some(descriptors) = self.continuation_descriptors {
            return Ok(descriptors);
        }
        let owner = self
            .callables
            .continuation_factory()
            .interface()
            .ok_or_else(|| declined!("a continuation object without its factory"))?;
        // Display text for the descriptor. The type test compares the descriptor, not this text.
        let qualified = owner.render().replace('/', ".");
        let simple = qualified
            .rsplit('.')
            .next()
            .unwrap_or(qualified.as_str())
            .to_string();
        let namespace = super::super::super::symbols::module_namespace(self.module_name);
        let interface_symbol = format!("kt_modtype_{namespace}_continuation_iface");
        let class_symbol = format!("kt_modtype_{namespace}_continuation");
        let interface = self.declare_module_data(&interface_symbol)?;
        let class = self.declare_module_data(&class_symbol)?;
        let superclass = self.import_data("kt_type_any")?;
        self.define_shared_type_descriptor(
            interface,
            "kt_continuation_iface",
            &format!(".data.rel.ro.{interface_symbol}"),
            super::objects::DescriptorShape {
                kotlin_name: &qualified,
                instance_size: 0,
                reference_offsets: &[],
                vtable: &[],
                itable: &[],
                instantiable: false,
                superclass,
                interfaces: &[],
                reference_target: None,
                walk: super::objects::WalkMembers::default(),
                qualified_name: Some(&qualified),
                simple_name: Some(&simple),
                class_names: 0,
            },
        )?;
        let references = [model::HEADER_SIZE, model::HEADER_SIZE + 8];
        let vtable = self.any_vtable()?;
        self.define_shared_type_descriptor(
            class,
            "kt_continuation",
            &format!(".data.rel.ro.{class_symbol}"),
            super::objects::DescriptorShape {
                kotlin_name: &qualified,
                instance_size: CONTINUATION_SIZE,
                reference_offsets: &references,
                vtable: &vtable,
                itable: &[],
                instantiable: true,
                superclass,
                interfaces: &[interface],
                reference_target: None,
                walk: super::objects::WalkMembers::default(),
                qualified_name: Some(&qualified),
                simple_name: Some(&simple),
                class_names: 0,
            },
        )?;
        self.continuation_descriptors = Some((class, interface));
        Ok((class, interface))
    }

    /// One stable instance of a dependency object whose published declaration stores nothing.
    pub(super) fn inert_instance(
        &mut self,
        classifier: krusty::types::TypeName,
    ) -> Result<DataId, Unsupported> {
        if let Some(instance) = self.inert_objects.get(&classifier) {
            return Ok(*instance);
        }
        let base = format!("kt_inert_{}", self.inert_objects.len());
        let descriptor = self.declare_local_data(&format!("kt_type_{base}"), false)?;
        let vtable = self.any_vtable()?;
        let superclass = self.import_data("kt_type_any")?;
        let kotlin_name = classifier.render().replace('/', ".");
        self.define_type_descriptor(
            descriptor,
            &base,
            super::objects::DescriptorShape {
                kotlin_name: &kotlin_name,
                instance_size: model::HEADER_SIZE,
                reference_offsets: &[],
                vtable: &vtable,
                itable: &[],
                instantiable: true,
                superclass,
                interfaces: &[],
                reference_target: None,
                walk: super::objects::WalkMembers::default(),
                qualified_name: Some(&kotlin_name),
                simple_name: None,
                class_names: 0,
            },
        )?;
        let instance = self.declare_local_data(&format!("{base}_instance"), false)?;
        let mut description = DataDescription::new();
        description.define(vec![0; model::HEADER_SIZE as usize].into_boxed_slice());
        description.set_align(8);
        let global = self
            .module
            .declare_data_in_data(descriptor, &mut description);
        description.write_data_addr(0, global, 0);
        self.module
            .define_data(instance, &description)
            .map_err(|error| format!("defining `{base}_instance` ({error})"))?;
        self.inert_objects.insert(classifier, instance);
        Ok(instance)
    }
}

/// The expression under representation casts. A cast to the completion's static type does not
/// make a different object into the factory's.
fn value_under_casts(ir: &IrFile, mut id: u32) -> u32 {
    loop {
        match ir.expr(id) {
            IrExpr::TypeOp {
                op:
                    IrTypeOp::ImplicitCoercion
                    | IrTypeOp::Cast
                    | IrTypeOp::CastNonNull
                    | IrTypeOp::SafeCast,
                arg,
                ..
            } => {
                id = *arg;
            }
            _ => return id,
        }
    }
}

/// Whether `id` is a call of the continuation factory.
fn factory_continuation(file: &FileLowering<'_>, id: u32) -> bool {
    let IrExpr::Call { callee, .. } = file.ir.expr(id) else {
        return false;
    };
    let Callee::External { target, .. } = &**callee else {
        return false;
    };
    file.callables.callable(*target).is_some_and(|fact| {
        fact.compiler_operation()
            == Some(krusty::backend::BackendCompilerIntrinsic::ContinuationFactory)
    })
}

/// The selected declaration's own operands: the classifier owning a member, the declared value
/// parameters, and the argument coordinate a serialized body keeps an extension receiver at.
#[derive(Clone, Copy)]
pub(super) struct DeclaredOperands<'a> {
    pub(super) receiver: Option<Ty>,
    pub(super) params: &'a [Ty],
    pub(super) extension_receiver_parameter: Option<u32>,
}

/// The two common-IR representations of one exact top-level extension receiver. A checked source
/// edge carries it separately from value arguments. A serialized KLIB support body retains it at
/// its authenticated parameter coordinate. No other coordinate is an extension operand.
fn exact_extension_operand(
    receiver: Option<u32>,
    args: &[u32],
    extension_receiver_parameter: Option<u32>,
) -> Option<u32> {
    match (receiver, args, extension_receiver_parameter) {
        (Some(value), [], None) => Some(value),
        (None, [value], Some(0)) => Some(*value),
        _ => None,
    }
}

/// An exact extension has one receiver and one value argument. The serialized form's
/// receiver remains at its authenticated parameter coordinate rather than becoming dispatch.
fn exact_extension_pair(
    receiver: Option<u32>,
    args: &[u32],
    extension_receiver_parameter: Option<u32>,
) -> Option<(u32, u32)> {
    match (receiver, args, extension_receiver_parameter) {
        (Some(receiver), [argument], None) => Some((receiver, *argument)),
        (None, [receiver, argument], Some(0)) => Some((*receiver, *argument)),
        _ => None,
    }
}

pub(super) fn console_intrinsic(
    intrinsic: Option<krusty::backend::BackendCompilerIntrinsic>,
) -> Option<super::super::super::intrinsics::ConsoleIntrinsic> {
    use super::super::super::intrinsics::ConsoleIntrinsic;
    use krusty::backend::BackendCompilerIntrinsic;

    match intrinsic {
        Some(BackendCompilerIntrinsic::Print) => Some(ConsoleIntrinsic::Print),
        Some(BackendCompilerIntrinsic::Println) => Some(ConsoleIntrinsic::Println),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{exact_extension_operand, exact_extension_pair};

    #[test]
    fn an_extension_pair_preserves_its_two_authenticated_operand_layouts() {
        assert_eq!(exact_extension_pair(Some(3), &[5], None), Some((3, 5)));
        assert_eq!(exact_extension_pair(None, &[7, 11], Some(0)), Some((7, 11)));
        for (receiver, args, coordinate) in [
            (None, &[7, 11][..], None),
            (None, &[7, 11][..], Some(1)),
            (None, &[7][..], Some(0)),
            (None, &[7, 11, 13][..], Some(0)),
            (Some(3), &[][..], None),
            (Some(3), &[5, 7][..], None),
            (Some(3), &[5][..], Some(0)),
        ] {
            assert_eq!(
                exact_extension_pair(receiver, args, coordinate),
                None,
                "receiver={receiver:?}, args={args:?}, coordinate={coordinate:?}"
            );
        }
    }

    #[test]
    fn a_floating_extension_accepts_only_its_two_authenticated_receiver_coordinates() {
        assert_eq!(exact_extension_operand(Some(3), &[], None), Some(3));
        assert_eq!(exact_extension_operand(None, &[5], Some(0)), Some(5));

        assert_eq!(exact_extension_operand(None, &[5], None), None);
        assert_eq!(exact_extension_operand(None, &[5], Some(1)), None);
        assert_eq!(exact_extension_operand(Some(3), &[5], None), None);
        assert_eq!(exact_extension_operand(Some(3), &[], Some(0)), None);
    }
}
