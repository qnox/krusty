//! Generic type-use attributes on fields and declaration-backed property accessors.

use super::{JvmSignatureFormatter, Wildcards};
use crate::jvm::classfile::ACC_FINAL;
use crate::types::{Ty, TyKind};

pub(in crate::jvm::ir_emit) struct JvmPropertySignatures {
    pub(in crate::jvm::ir_emit) getter: Option<String>,
    pub(in crate::jvm::ir_emit) setter: Option<String>,
}

fn retains_generic_type_use(ty: Ty) -> bool {
    match ty.kind() {
        TyKind::Nullable(inner)
        | TyKind::PlatformNullable(inner)
        | TyKind::MutabilityFlexible(inner) => retains_generic_type_use(inner),
        TyKind::TyParam(..) | TyKind::Fun(_) | TyKind::Intersection(_) => true,
        TyKind::Obj(_, arguments) => !arguments.is_empty(),
        _ => false,
    }
}

impl JvmSignatureFormatter<'_> {
    /// A retained semantic formal may belong to an enclosing method rather than this classifier.
    /// Its declaration identity still formats through the same signature boundary as an own formal.
    pub(in crate::jvm::ir_emit) fn declaration_type_use(
        &self,
        ty: &Ty,
        wildcards: Wildcards,
    ) -> Option<String> {
        if !retains_generic_type_use(*ty) {
            return None;
        }
        let signature = self.ty_at(ty, wildcards)?;
        (signature != super::super::ir_type_desc(ty)).then_some(signature)
    }

    fn declaration_method_type_use(&self, params: &[Ty], ret: &Ty) -> Option<String> {
        if !params
            .iter()
            .chain(std::iter::once(ret))
            .copied()
            .any(retains_generic_type_use)
        {
            return None;
        }
        let signature = self.method_positions(params, ret)?;
        (signature != super::super::ir_method_desc(params, ret)).then_some(signature)
    }

    /// A backing field's generic `Signature`. kotlinc maps a field's type in its return-type mode
    /// when the field is final and in its value-parameter mode otherwise (`mapFieldSignature`): a
    /// `val xs: List<CharSequence>` signs `Ljava/util/List<Ljava/lang/CharSequence;>;`, a `var`
    /// `Ljava/util/List<+Ljava/lang/CharSequence;>;`.
    pub(in crate::jvm::ir_emit) fn field_signature(
        &self,
        ty: &Ty,
        type_parameter: Option<&str>,
        access: u16,
    ) -> Option<String> {
        if let Some(parameter) = type_parameter {
            return Some(format!("T{parameter};"));
        }
        let wildcards = if access & ACC_FINAL != 0 {
            Wildcards::Suppressed
        } else {
            Wildcards::Declared
        };
        self.declaration_type_use(ty, wildcards)
    }

    pub(in crate::jvm::ir_emit) fn property_signatures(
        &self,
        ty: &Ty,
        type_parameter: Option<&str>,
    ) -> JvmPropertySignatures {
        if let Some(parameter) = type_parameter {
            return JvmPropertySignatures {
                getter: Some(format!("()T{parameter};")),
                setter: Some(format!("(T{parameter};)V")),
            };
        }
        JvmPropertySignatures {
            getter: self.declaration_method_type_use(&[], ty),
            setter: self.declaration_method_type_use(std::slice::from_ref(ty), &Ty::Unit),
        }
    }
}
