//! Immutable lexical values become the existing common-IR closure capture prefix.

use super::super::function_lowering::{closure_header, declare_function, CaptureParameter};
use super::*;
use crate::ir::{IrCapturedDeclaration, IrParameterIdentity, IrValueCapture};
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
        if signature.suspend {
            return unsupported("a suspend closure");
        }
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
        let header = closure_header(self.arena, function, self.header, &captures)?;
        if header.params()[captures.len()..] != *signature.params
            || header.result() != signature.ret
        {
            return Err(mismatch(
                "a closure's declaration disagrees with its function type",
            ));
        }
        let arity = u8::try_from(signature.params.len())
            .map_err(|_| mismatch("a closure has too many invocation parameters"))?;
        let implementation = declare_function(self.ir, &header);
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
        lowering.initialization = self.initialization;
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
}
