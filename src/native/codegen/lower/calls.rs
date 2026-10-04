//! Calls selected by checked common IR, realized through native symbols and dispatch.
//!
//! This module owns the call boundary: local/direct/virtual calls, compiler intrinsics, and the
//! mapping from a frozen dependency-callable fact to the native runtime operation that implements
//! it. Name-based runtime tables remain explicit migration debt inside `native::intrinsics`.

use super::*;

impl BodyLowering<'_, '_, '_> {
    pub(super) fn call(
        &mut self,
        site: u32,
        callee: &Callee,
        dispatch_receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        match callee {
            Callee::LocalWithDefaults { function, defaults }
            | Callee::ClassStaticWithDefaults {
                function, defaults, ..
            } => self.defaulted_call(*function, defaults, dispatch_receiver, args),
            Callee::ModuleWithDefaults { defaults, .. } => {
                match self.file.inherited_default_provider(callee) {
                    Some(provider) => {
                        self.defaulted_call(provider, defaults, dispatch_receiver, args)
                    }
                    None => Err(format!("a {} call", callee_kind(callee))),
                }
            }
            // A static method owned by a class is, to this generator, a function with a symbol —
            // the owner is a JVM placement fact, and there is no flat facade here for it to be
            // placed differently from. A local function declared inside a member is the shape that
            // arrives this way.
            Callee::Local(function) | Callee::ClassStatic { function, .. } => {
                if dispatch_receiver.is_some() {
                    return Err("a static call with a receiver".to_string());
                }
                let params =
                    super::super::super::captures::carried_parameters(self.file.ir, *function);
                let arguments = self.arguments(args, &params)?;
                if self.terminated {
                    return Ok(None);
                }
                let Some(id) = self.file.functions[*function as usize] else {
                    return Err(format!(
                        "a call to `{}`, which has no body",
                        self.file.ir.functions[*function as usize].name
                    ));
                };
                let func_ref = self.func_ref(id);
                let call = self.emit_call(func_ref, &arguments)?;
                Ok(self.builder.inst_results(call).first().copied())
            }
            Callee::Super {
                owner,
                name,
                kind,
                declaration,
                ..
            } => {
                let Some(receiver) = dispatch_receiver else {
                    return Err(format!("a `super` call without a receiver (`{name}`)"));
                };
                let target = objects::SuperTarget {
                    owner: *owner,
                    name,
                    kind: *kind,
                    declaration: *declaration,
                };
                self.direct_call(target, receiver, args)
            }
            Callee::Virtual {
                owner,
                name,
                params,
                ..
            } => {
                let Some(receiver) = dispatch_receiver else {
                    return Err(format!("a virtual call without a receiver (`{name}`)"));
                };
                self.virtual_call(*owner, name, params.as_ref(), receiver, args)
            }
            Callee::Special {
                owner,
                name,
                source,
                ..
            } => {
                let Some(receiver) = dispatch_receiver else {
                    return Err(format!("a `super` call without a receiver (`{name}`)"));
                };
                // A diamond `super.f()` to a superinterface's DEFAULT METHOD. `Callee::Special`
                // carries no accessor kind because it never names one.
                let target = objects::SuperTarget {
                    owner: *owner,
                    name,
                    kind: crate::ir::IrSuperCallKind::Function,
                    declaration: source.map(crate::fir::ResolvedFunctionOverrideTarget::Module),
                };
                self.direct_call(target, receiver, args)
            }
            Callee::Intrinsic { operation, ret } => {
                self.intrinsic(*operation, *ret, dispatch_receiver, args)
            }
            Callee::External {
                target,
                params,
                ret,
                ..
            } => {
                let Some(realization) = self.file.callables.callable(*target) else {
                    return Err("an unresolvable dependency call".to_string());
                };
                // A callable reference reaches this ordinary dependency-call path, while retaining
                // the exact compiler intrinsic the source-form operation uses.
                if let Some(realized) = self.compiler_intrinsic_call(
                    realization.compiler_intrinsic,
                    dispatch_receiver,
                    args,
                    *ret,
                ) {
                    return realized;
                }
                let owner = super::super::super::intrinsics::DeclarationOwner::callable(
                    realization.physical_owner,
                    realization.declaration_package,
                );
                // The Kotlin name the declaration PUBLISHES, not the spelling it is realized under.
                // A physical name is an emit handle: a JVM realization may RENAME a member, and
                // where the signature mentions a value class kotlinc appends a hash of the erasure
                // (`UInt.compareTo` is realized as `compareTo-WZ4Q5Ns`). Neither is recoverable
                // from the spelling, and neither has to be: the contract carries the Kotlin name
                // beside it. Where it does not, the member declines rather than being guessed at.
                let name = realization
                    .reflection_name
                    .clone()
                    .unwrap_or_else(|| realization.name.clone());
                match dispatch_receiver {
                    // A member: the receiver is the runtime function's first argument, and
                    // everything crosses as a reference.
                    Some(receiver) => {
                        // A receiver typed by a RUNTIME-KNOWN type this file puts a class of its
                        // own behind may be one of those objects, and every table below answers
                        // only for the ones the runtime MAKES. No static type tells the two apart
                        // — that is why those answers are the runtime's at all — so the member
                        // declines by name, with the type it was asked of still in sight.
                        if let Some(internal) = self
                            .type_of(receiver)
                            .map(Ty::non_null)
                            .and_then(|ty| ty.obj_internal())
                        {
                            if self.file.implements_dependency(internal) {
                                return Err(format!(
                                    "the member `{}.{name}` of a type this file implements itself",
                                    internal.render().replace('/', ".")
                                ));
                            }
                        }
                        if let Some(realized) =
                            self.scope_function(owner, &name, receiver, args, *ret)
                        {
                            return realized;
                        }
                        // `x.isNaN()` and its two siblings are one comparison each. Realizing them
                        // here rather than in the runtime keeps the operand unboxed — the member
                        // path below crosses everything as a reference, which for a `Double` would
                        // mean allocating a box to ask a question about its bits.
                        if let Some(predicate) =
                            super::super::super::intrinsics::float_predicate(owner, &name)
                        {
                            return self.float_predicate(predicate, receiver);
                        }
                        // The unsigned integers: a value class the erasure made look like the
                        // signed number sharing its bits, so every member where that difference
                        // shows is answered on purpose rather than by the signed instruction.
                        if let Some(element) = self.type_of(receiver) {
                            if let Some(realized) =
                                self.unsigned_member(element, &name, params, *ret, receiver, args)
                            {
                                return realized;
                            }
                        }
                        if let Some(realized) =
                            self.reference_member(realization.semantic_role, receiver, args, *ret)
                        {
                            return realized;
                        }
                        if let Some(realized) =
                            self.reference_delegate(realization.semantic_role, receiver, args, *ret)
                        {
                            return realized;
                        }
                        // `x++` where `x` is an `Int?`: the member is the primitive's, and so is
                        // the value, whatever it arrived carried as.
                        if let Some(realized) = self.boxed_step(owner, &name, params, receiver) {
                            return realized;
                        }
                        // `42.toUInt()`: a SIGNED receiver converted to an unsigned type.
                        // Nothing is called — Kotlin defines the conversion as the ordinary signed
                        // one to the target's width with those bits reinterpreted, and this
                        // backend already carries an unsigned value as the machine integer it
                        // wraps, so the reinterpretation is not an operation at all.
                        //
                        // The receiver is read at ITS OWN type, which is what makes the answer
                        // right: `convert` resizes by the SOURCE's signedness, so a negative
                        // `Int` widening to `ULong` sign-extends (kotlinc answers
                        // 18446744073709551615, not 4294967295) while a wide source narrowing
                        // truncates. Only an integer source is taken; a float one saturates
                        // instead and `unsigned_conversion` says why it is not here.
                        if let Some(target) =
                            super::super::super::intrinsics::unsigned_conversion(owner, &name)
                        {
                            let source = self.type_of(receiver).map(Ty::non_null);
                            if let Some(source @ (Ty::Byte | Ty::Short | Ty::Int | Ty::Long)) =
                                source
                            {
                                let Some(value) = self.coerce(receiver, source)? else {
                                    return Ok(None);
                                };
                                if self.terminated {
                                    return Ok(None);
                                }
                                let Some(produced) = self.convert(value, Some(source), target)?
                                else {
                                    return Ok(None);
                                };
                                return self.convert(produced, Some(target), *ret);
                            }
                        }
                        // `a.mod(b)`: the remainder brought onto the DIVISOR's sign. Both
                        // operands are read at the width the declaration answers in, which is what
                        // makes `Int.mod(Long)` a `Long` question rather than a truncated one.
                        let receiver_type = self.type_of(receiver).map(Ty::non_null);
                        if let Some((symbol, operand)) = receiver_type.and_then(|receiver_type| {
                            super::super::super::intrinsics::floor_mod(
                                owner,
                                &name,
                                receiver_type,
                                params,
                            )
                        }) {
                            let [argument] = args else {
                                return Err("a `mod` with more than one operand".to_string());
                            };
                            let Some(left) = self.coerce(receiver, operand)? else {
                                return Err("a `Unit` receiver for `mod`".to_string());
                            };
                            let Some(right) = self.coerce(*argument, operand)? else {
                                return Err("a `Unit` operand for `mod`".to_string());
                            };
                            if self.terminated {
                                return Ok(None);
                            }
                            let produced = self.runtime_call(
                                symbol,
                                &[operand, operand],
                                operand,
                                &[left, right],
                            )?;
                            let Some(produced) = produced else {
                                return Ok(None);
                            };
                            return self.convert(produced, Some(operand), *ret);
                        }
                        // `kotlin.experimental`'s bit operations on the narrow integers. Kotlin
                        // gives `Int` and `Long` the same four as members and these as extensions,
                        // which is where the library put them rather than a difference in what
                        // they mean — so they are instructions here, not a call, which is also why
                        // they are not in `scalar_member`: that table boxes its receiver.
                        if let Some(op) = super::super::super::intrinsics::experimental_bitwise(
                            owner, &name, params,
                        ) {
                            return self.experimental_bitwise(op, receiver, args, *ret);
                        }
                        // `s.startsWith(t)`, `s.endsWith(t)` and `t in s`, whose last parameter
                        // is Kotlin's `ignoreCase`. The default reaches here as a CONSTANT
                        // argument rather than as an absent one, so the case-sensitive form — the
                        // only one the runtime answers — is recognizable right here: a literal
                        // `false` and nothing else. Anything else asks about Unicode case folding,
                        // which the runtime holds no table for, and declines below by name.
                        if let Some(symbol) =
                            super::super::super::intrinsics::case_sensitive_text_member(
                                owner, &name, params,
                            )
                        {
                            // `ignoreCase` has a DEFAULT, and the two providers hand that over
                            // differently: a klib call materializes the default as a constant
                            // argument, a jar call leaves the argument out. Both mean the same
                            // thing — the case-sensitive form — and reading the argument list
                            // rather than the signature is what makes them the same answer.
                            let sensitive = match args.len() {
                                given if given + 1 == params.len() => true,
                                given if given == params.len() => args.last().is_some_and(|flag| {
                                    matches!(
                                        self.file.ir.expr(*flag),
                                        IrExpr::Const(IrConst::Boolean(false))
                                    )
                                }),
                                _ => false,
                            };
                            if sensitive {
                                let arguments =
                                    vec![self.reference(receiver)?, self.reference(args[0])?];
                                if self.terminated {
                                    return Ok(None);
                                }
                                let produced = self.runtime_call(
                                    symbol,
                                    &[any(), any()],
                                    Ty::Boolean,
                                    &arguments,
                                )?;
                                let Some(produced) = produced else {
                                    return Ok(None);
                                };
                                return self.convert(produced, Some(Ty::Boolean), *ret);
                            }
                        }
                        // `Float.fromBits(n)`: an extension of the COMPANION object, so the
                        // receiver is that object and nothing reads it — there is no object to
                        // make and none is made. The operand's width says which of the two.
                        if let Some((symbol, operand, answer)) =
                            super::super::super::intrinsics::bits_to_float(owner, &name, params)
                        {
                            let Some(bits) = self.coerce(args[0], operand)? else {
                                return Ok(None);
                            };
                            if self.terminated {
                                return Ok(None);
                            }
                            let produced =
                                self.runtime_call(symbol, &[operand], answer, &[bits])?;
                            let Some(produced) = produced else {
                                return Ok(None);
                            };
                            return self.convert(produced, Some(answer), *ret);
                        }
                        // `x.toBits()` / `x.toRawBits()`: an extension whose receiver is a machine
                        // value, taken at its own width rather than through a box. The receiver's
                        // type says which width, the declaration having no parameter to say it.
                        if let Some(receiver_ty) = self.type_of(receiver) {
                            if let Some((symbol, answer)) =
                                super::super::super::intrinsics::float_to_bits(
                                    owner,
                                    &name,
                                    params,
                                    receiver_ty,
                                )
                            {
                                let carried = receiver_ty.non_null();
                                let Some(value) = self.coerce(receiver, carried)? else {
                                    return Ok(None);
                                };
                                if self.terminated {
                                    return Ok(None);
                                }
                                let produced =
                                    self.runtime_call(symbol, &[carried], answer, &[value])?;
                                let Some(produced) = produced else {
                                    return Ok(None);
                                };
                                return self.convert(produced, Some(answer), *ret);
                            }
                        }
                        // `x.compareTo(y)` where the static type says only `Comparable`. The
                        // receiver's DESCRIPTOR says what to compare, exactly as `equals` and
                        // `toString` on such a receiver already read it — a boxed primitive at its
                        // own width, with Kotlin's TOTAL order for the floating ones, and a string
                        // by UTF-16 unit. A file that declares a `Comparable` of its own keeps
                        // declining: an object of the program's could stand behind that type and
                        // the runtime has no order for it.
                        if realization.semantic_role
                            == Some(
                                crate::backend::BackendSemanticCallRole::KotlinComparableCompareTo,
                            )
                            && !self.file.declares_its_own_comparable
                        {
                            let operands =
                                vec![self.reference(receiver)?, self.reference(args[0])?];
                            if self.terminated {
                                return Ok(None);
                            }
                            let produced = self.runtime_call(
                                "kt_compare_any",
                                &[any(), any()],
                                Ty::Int,
                                &operands,
                            )?;
                            let Some(produced) = produced else {
                                return Ok(None);
                            };
                            return self.convert(produced, Some(Ty::Int), *ret);
                        }
                        // A member that asks about a NUMBER rather than an object, carried as one:
                        // `s[i]` must not box its index to reach the runtime.
                        if let Some((symbol, carried, answer)) =
                            super::super::super::intrinsics::scalar_member(owner, &name, params)
                        {
                            let mut arguments = vec![self.reference(receiver)?];
                            for (argument, ty) in args.iter().zip(&carried[1..]) {
                                let Some(value) = self.coerce(*argument, *ty)? else {
                                    return Ok(None);
                                };
                                arguments.push(value);
                            }
                            if self.terminated {
                                return Ok(None);
                            }
                            let produced =
                                self.runtime_call(symbol, &carried, answer, &arguments)?;
                            // The table says what the runtime function PHYSICALLY answers; the
                            // call node says what the site expects. Reconciling the two is this
                            // boundary's job rather than something to assume: they are different
                            // sources and nothing here made them agree.
                            //
                            // They disagree today for `Number.toByte`/`toShort`, which the
                            // provider provider types `Int` because `desc_to_ty` reads the JVM
                            // descriptors `B` and `S` as `Int` — a core defect with its own fix,
                            // invisible to the JVM backend because a byte and an int share a stack
                            // slot there. Without this, the i8 the runtime answers reaches a box
                            // helper that takes an i32 and Cranelift's verifier rejects the
                            // function. When the provider is fixed the coercion becomes a no-op.
                            let Some(produced) = produced else {
                                return Ok(None);
                            };
                            return self.convert(produced, Some(answer), *ret);
                        }
                        // A companion member the runtime realizes takes its arguments alone:
                        // the receiver is an object carrying nothing, and it is not evaluated.
                        if let Some(symbol) =
                            super::super::super::intrinsics::runtime_companion_member(
                                owner, &name, params,
                            )
                        {
                            let mut arguments = Vec::with_capacity(args.len());
                            for argument in args {
                                arguments.push(self.reference(*argument)?);
                            }
                            if self.terminated {
                                return Ok(None);
                            }
                            let signature = vec![any(); arguments.len()];
                            let produced =
                                self.runtime_call(symbol, &signature, any(), &arguments)?;
                            let Some(produced) = produced else {
                                return Ok(None);
                            };
                            return self.convert(produced, Some(any()), *ret);
                        }
                        let Some(symbol) = super::super::super::intrinsics::runtime_member(
                            owner,
                            &name,
                            params,
                            compiler_intrinsics::runtime_member_role(
                                realization.compiler_intrinsic,
                                self.file
                                    .ir
                                    .semantic_call_roles
                                    .get(&site)
                                    .copied()
                                    .or(realization.semantic_role),
                            ),
                        ) else {
                            return Err(format!(
                                "the member `{}.{name}`",
                                realization.physical_owner.render().replace('/', ".")
                            ));
                        };
                        let mut arguments = vec![self.reference(receiver)?];
                        for argument in args {
                            arguments.push(self.reference(*argument)?);
                        }
                        if self.terminated {
                            return Ok(None);
                        }
                        let signature = vec![any(); arguments.len()];
                        self.runtime_call(symbol, &signature, *ret, &arguments)
                    }
                    None => {
                        if let Some(operation) =
                            compiler_intrinsics::console_intrinsic(realization.compiler_intrinsic)
                        {
                            let Some(symbol) = super::super::super::intrinsics::console_intrinsic(
                                operation, params,
                            ) else {
                                return Err(format!("a malformed `{operation:?}` call"));
                            };
                            let arguments = self.arguments(args, params)?;
                            if self.terminated {
                                return Ok(None);
                            }
                            return self.runtime_call(&symbol, params, *ret, &arguments);
                        }
                        // `kotlin.test`'s assertions. Their operands cross as REFERENCES rather
                        // than at their own widths: `assertEquals` is generic, so a call with
                        // `Int` arguments arrives typed `Int`, and the comparison Kotlin makes is
                        // `==` on whatever the values are.
                        if let Some((symbol, compared)) =
                            super::super::super::intrinsics::assertion_call(owner, &name, params)
                        {
                            return self.assertion(symbol, compared, args);
                        }
                        if super::super::super::intrinsics::is_assert_fails_with(
                            owner, &name, params,
                        ) {
                            return self.assert_fails_with(args, params, *ret);
                        }
                        if let Some(realized) =
                            self.top_level_scope_function(owner, &name, args, params, *ret)
                        {
                            return realized;
                        }
                        if let Some(builder) =
                            super::super::super::intrinsics::builder_scope(owner, &name, params)
                        {
                            return self.builder_scope_function(builder, args);
                        }
                        // `require`, `check`, `requireNotNull`, `checkNotNull`, `error`. Not a
                        // runtime call: the message block runs only when the check fails, so the
                        // shape is a branch around a raise rather than a call with operands.
                        if let Some(precondition) =
                            super::super::super::intrinsics::precondition(owner, &name, params)
                        {
                            return self.precondition(precondition, args, *ret);
                        }
                        let Some(symbol) =
                            super::super::super::intrinsics::runtime_function(owner, &name, params)
                        else {
                            return Err(format!(
                                "the declaration `{}.{name}`",
                                realization.physical_owner.render().replace('/', ".")
                            ));
                        };
                        let arguments = self.arguments(args, params)?;
                        if self.terminated {
                            return Ok(None);
                        }
                        self.runtime_call(&symbol, params, *ret, &arguments)
                    }
                }
            }
            other => Err(format!("a {} call", callee_kind(other))),
        }
    }
}
