//! Immutable lexical values become the existing common-IR closure capture prefix.

use super::super::function_lowering::{closure_header, declare_function, CaptureParameter};
use super::*;
use crate::ir::type_reflection::type_parameters_named_by;
use crate::ir::{IrCapturedDeclaration, IrParameterIdentity, IrTypeParameter, IrValueCapture};
use crate::metadata::klib_ir::tree::{KlibIrBody, KlibIrFunctionId};
use crate::types::TyKind;

impl BodyLowering<'_> {
    pub(super) fn closure(
        &mut self,
        function: KlibIrFunctionId,
        ty: Ty,
        source_form: Option<&str>,
    ) -> Lowered<ExprId> {
        let TyKind::Fun(signature) = ty.kind() else {
            return unsupported("a closure of a non-function type");
        };
        // The serialized source-form marker owns this distinction. Neither the expression's
        // encoding nor the FunctionN.invoke override proves whether source used `fun` or braces.
        let capturer = match super::super::closure_forms::source_form(source_form) {
            Some(capturer) => capturer,
            None => return unsupported("a closure without a serialized source form"),
        };
        let function = self.arena.function(function);
        let statements = match &function.body {
            Some(KlibIrBody::Block(statements)) => statements,
            Some(KlibIrBody::Synthetic(_)) => return Err(KlibBodyDeclineReason::SyntheticBody),
            None => return Err(KlibBodyDeclineReason::NoBody),
        };
        let mut captures = Vec::new();
        let mut operands = Vec::new();
        let mut receivers = Vec::new();
        let mut seen = HashSet::default();
        for symbol in self.arena.lexical_value_symbols(function) {
            if !seen.insert(symbol.clone()) {
                continue;
            }
            let ordinal = u32::try_from(captures.len()).expect("a capture list fits u32");
            let mut capture = if let Some(local) = self.locals.get(symbol) {
                if local.mutable {
                    return unsupported("a mutable closure capture");
                }
                CaptureParameter {
                    symbol: symbol.clone(),
                    ty: local.ty,
                    identity: IrParameterIdentity::captured_value(
                        self.locals.name(symbol),
                        ordinal,
                        IrValueCapture {
                            declaration: IrCapturedDeclaration::Variable,
                            capturer,
                            crossinline_parameter: None,
                        },
                    ),
                    receiver: None,
                }
            } else if let Some(parameter) = self.header.capture_parameter(
                symbol,
                ordinal,
                capturer,
                &self
                    .ir
                    .fn_params
                    .get(&self.function.function)
                    .expect("a body publishes its parameters")
                    .captured_receivers,
                self.ir
                    .lambda_own_params_from
                    .contains_key(&self.function.function),
            )? {
                parameter
            } else if let Some(alias) = self.compiler_aliases.get(symbol) {
                CaptureParameter {
                    symbol: symbol.clone(),
                    ty: self.ty(self.arena.expr(*alias), "capture")?,
                    identity: IrParameterIdentity::captured_value(
                        None,
                        ordinal,
                        IrValueCapture {
                            declaration: IrCapturedDeclaration::Variable,
                            capturer,
                            crossinline_parameter: None,
                        },
                    ),
                    receiver: None,
                }
            } else {
                // The symbol belongs to this closure or one of its nested declarations. Its own
                // frame authenticates the read; it is not an input from the enclosing frame.
                continue;
            };
            if let Some(receiver) = capture.receiver.take() {
                capture.identity = IrParameterIdentity::captured_receiver(
                    u32::try_from(receivers.len()).expect("a receiver list fits u32"),
                );
                receivers.push(receiver);
            }
            let read = self.read(symbol)?;
            // A prepared enclosing body may read an erased carrier. The closure stores the
            // declaration's value, using the ordinary conversion back to that declared type.
            let read = match self.header.value(symbol) {
                Some(value) if value.erased.is_some() => self.coercion(read, capture.ty),
                _ => read,
            };
            operands.push(self.typed(read, capture.ty));
            captures.push(capture);
        }
        // The closure's parameters and result may name a class private to the body's library,
        // as the body's own expressions do.
        let mut types = self.header.type_parameters().clone();
        super::super::private_signature_classes::bind_with(
            &mut types,
            self.arena,
            function,
            &|signature| self.linker.local_class(signature),
        )?;
        let header = closure_header(self.arena, function, types, &captures)?;
        if header.params()[captures.len()..] != *signature.params
            || header.result() != signature.ret
        {
            return Err(mismatch(
                "a closure's declaration disagrees with its function type",
            ));
        }
        // A suspend lambda's invocation takes its continuation after its parameters; common CPS
        // lowering adds that parameter to the implementation it records as a suspend function.
        let arity = signature
            .params
            .len()
            .checked_add(usize::from(signature.suspend))
            .and_then(|arity| u8::try_from(arity).ok())
            .ok_or_else(|| mismatch("a closure has too many invocation parameters"))?;
        let type_parameters = self.closure_type_parameters(ty)?;
        let implementation = declare_function(self.ir, &header);
        self.ir
            .record_lambda_type_parameters(implementation, type_parameters);
        if signature.suspend {
            self.ir.suspend_funs.push(implementation);
        }
        self.ir.lambda_own_params_from.insert(
            implementation,
            u32::try_from(captures.len()).expect("a capture list fits u32"),
        );
        self.ir
            .set_method_visibility(implementation, crate::types::Visibility::Private);
        self.ir
            .fn_params
            .get_mut(&implementation)
            .expect("a closure publishes its parameters")
            .captured_receivers = receivers;
        // The body may return from this declaration when the closure is inlined into it. An
        // inline function's own body is expanded before the closures it passes, so a return from
        // it stays unsupported; so does one from an object literal's member, or from a body whose
        // result is not its declared one.
        let mut enclosing_returns = self.enclosing_returns.clone();
        if self.member_return.is_none()
            && !self.ir.inline_only_fns.contains(&self.function.function)
            && !self.header.erases_result()
            && self.header.body_result() == self.header.result()
        {
            enclosing_returns.push((self.function.symbol.clone(), self.header.result()));
        }
        let mut lowering = BodyLowering::new(
            self.arena,
            LoweredFunction {
                symbol: &function.base.symbol,
                function: implementation,
            },
            &header,
            self.builtins,
            self.ir,
            self.linker,
        );
        lowering.instance_storage = self
            .instance_storage
            .as_ref()
            .and_then(super::instance_storage::InstanceStorageFrame::nested);
        lowering.enclosing_returns = enclosing_returns;
        let body = lowering.body(statements)?;
        self.ir.functions[implementation as usize].body = Some(body);
        Ok(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity,
            captures: operands,
            sam: None,
            inline_body: None,
        }))
    }

    /// The declaration-owned formals the closure's function type `ty` names, closed over their
    /// bounds in first-use order, as checked FIR records a source lambda's. The enclosing
    /// declarations bound each of them with its declared bounds and variance.
    fn closure_type_parameters(&mut self, ty: Ty) -> Lowered<Vec<IrTypeParameter>> {
        let mut names = Vec::new();
        type_parameters_named_by(ty, &mut names);
        let header = self.header;
        let types = header.type_parameters();
        let mut parameters = Vec::with_capacity(names.len());
        let mut next = 0;
        while let Some(&name) = names.get(next) {
            next += 1;
            let Some(declared) = types.declared_bounds(name) else {
                return Err(mismatch(
                    "a closure's function type names a formal no enclosing declaration declares",
                ));
            };
            let mut bounds = Vec::with_capacity(declared.len());
            for &bound in declared {
                type_parameters_named_by(bound, &mut names);
                // A bound naming another type parameter names no classifier.
                let interface = match bound.non_null().kotlin_class_internal() {
                    Some(classifier) => self.linker.is_interface(classifier)?,
                    None => false,
                };
                bounds.push((bound, interface));
            }
            parameters.push(IrTypeParameter {
                name: crate::types::type_parameter_source_name(name).to_owned(),
                semantic_name: name.to_owned(),
                bounds,
                variance: types.declared_variance(name),
                // A serialized body with a reified formal is declined where it is bound.
                reified: false,
                captured_from_enclosing: false,
            });
        }
        Ok(parameters)
    }
}
