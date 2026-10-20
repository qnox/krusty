//! Calls of dependency constructors and reads of dependency singletons.
//!
//! A constructor call lowers as checked FIR lowers the source construction of a dependency class:
//! one construction of the class by the provider's constructor identity, with its supplied operands
//! in declaration order and the ordinals of the parameters it leaves to their defaults. An object
//! value lowers as the source reference of the singleton: its classifier's singleton value.

use super::calls::arguments;
use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr};
use crate::metadata::klib_ir::tree::KlibIrMemberAccess;
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::Ty;

impl BodyLowering<'_> {
    /// The value of the dependency object `symbol`, typed `ty`: the object's own class type.
    pub(super) fn object_value(&mut self, symbol: &KlibIrSymbol, ty: Ty) -> Lowered<ExprId> {
        if symbol.kind != KlibIrSymbolKind::Class {
            return Err(mismatch("an object value's symbol is not a class"));
        }
        let KlibIrSignature::Public(object) = &symbol.signature else {
            return unsupported("a file-private object value");
        };
        let classifier = super::super::klib_types::classifier_name(object)?;
        let object = crate::types::builtin_semantic(classifier).unwrap_or(Ty::obj_name(classifier));
        if ty != object {
            return Err(mismatch("an object value is typed apart from its object"));
        }
        self.linker.singleton(classifier)?;
        Ok(self.ir.add_expr(IrExpr::SingletonValue { classifier }))
    }

    pub(super) fn constructor_call(
        &mut self,
        access: &KlibIrMemberAccess,
        constructor_type_arguments: u32,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if access.symbol.kind != KlibIrSymbolKind::Constructor {
            return Err(mismatch("a constructor call's symbol is not a constructor"));
        }
        let KlibIrSignature::Public(callee) = &access.symbol.signature else {
            return unsupported("a call of a file-private constructor");
        };
        if constructor_type_arguments != 0 {
            return unsupported("a call of a generic constructor");
        }
        let linked = self.linker.link_constructor(callee)?;
        if !matches!(ty, Ty::Obj(classifier, _) if classifier == linked.classifier) {
            return Err(mismatch(
                "a constructor call is typed apart from its constructor's class",
            ));
        }
        let arguments = arguments(access);
        if arguments.len() != linked.params.len() {
            return Err(mismatch(
                "a constructor call passes another number of arguments than its constructor declares",
            ));
        }
        let mut args = Vec::with_capacity(arguments.len());
        let mut defaults = Vec::new();
        for (ordinal, (argument, parameter)) in
            arguments.into_iter().zip(&linked.params).enumerate()
        {
            match argument {
                Some(argument) => args.push(self.operand(argument, *parameter)?),
                None => defaults.push(ordinal as u32),
            }
        }
        Ok(self.ir.add_expr(IrExpr::New {
            internal: linked.classifier,
            args,
            ctor_params: Some(linked.params),
            ctor_desc: None,
            external_target: Some(linked.target),
            defaults: defaults.into_boxed_slice(),
            default_prefix_count: 0,
        }))
    }
}
