//! `typeOf<T>()`: a new `KType` object describing the checked type argument
//! (the Wasm runtime's `type_objects`).
//!
//! The described classifier is its identity: a declared classifier's name, or the arity of a
//! `FunctionN`, which a function type and its `FunctionN` spelling share. Its Kotlin qualified
//! name is only what Kotlin/Wasm prints. A local class or an anonymous object
//! has none, and a type parameter's classifier is a `KTypeParameter`, which this target does not
//! build; each declines, as does a suspend function type.

use super::super::super::type_objects::{Classifier, Projection, TypeDescription};
use super::*;
use krusty::backend::class_tables::AnyMember;
use krusty::ir::ClassId;

impl Body<'_> {
    /// Push the `KType` the `typeOf` call of `target` makes.
    pub(super) fn type_of_call(
        &mut self,
        target: krusty::fir::ExternalCallableId,
        substitutions: &[krusty::ir::IrCheckedSubstitution],
    ) -> Result<(), Unsupported> {
        let argument = substitutions
            .iter()
            .find(|substitution| {
                matches!(
                    substitution.parameter,
                    krusty::fir::FirTypeParameterRef::External { callable, ordinal: 0 }
                        if callable == target
                )
            })
            .ok_or("a `typeOf` call without its checked type argument")?;
        let description = self.type_description(argument.value)?;
        let object = self.runtime.type_object(self.module, description)?;
        self.code.global_get(object.table).i32_const(object.number);
        let text = object.text.encode_utf16().collect::<Vec<_>>();
        self.runtime
            .string_constant(self.module, &mut self.code, &text);
        self.code.struct_new(object.structure);
        Ok(())
    }

    /// Replace the `KType` reference on the stack, statically of type `ty`, with its `toString()`.
    /// The same reach analysis as a template `toString` of an erased reference: a slot with no
    /// body declines, and a realized `kotlin.Any.toString` does not.
    pub(super) fn type_object_to_string(&mut self, ty: Ty) -> Result<(), Unsupported> {
        if self.may_reach_unrealized(ty, AnyMember::ToString)? {
            return Err(format!(
                "a `toString` of `{}` reaching a runtime object this target cannot print",
                describe(ty)
            ));
        }
        self.dispatch_to_string();
        Ok(())
    }

    /// Whether a class of this file implements `KType` and has no body for `member`. The runtime's
    /// own `KType` object answers every member, so only those classes can.
    pub(super) fn ktype_reaches_unrealized(&self, member: AnyMember) -> Result<bool, Unsupported> {
        let Some(classes) = self.classes else {
            return Ok(false);
        };
        Ok(self.ir.classes.iter().enumerate().any(|(id, class)| {
            !class.is_interface
                && self.is_ktype_subtype(class.fq_name, &mut Vec::new())
                && !classes.realizes(id as ClassId, member)
        }))
    }

    /// Whether this file's classifier `name` is `KType` or a subtype of it, through the
    /// supertypes this file declares.
    fn is_ktype_subtype(&self, name: krusty::types::TypeName, seen: &mut Vec<ClassId>) -> bool {
        if name == super::super::super::type_objects::ktype() {
            return true;
        }
        let Some(id) = self.ir.class_id_by_name(name) else {
            return false;
        };
        if seen.contains(&id) {
            return false;
        }
        seen.push(id);
        let class = &self.ir.classes[id as usize];
        std::iter::once(class.superclass)
            .chain(class.interfaces.iter_ids())
            .chain(
                class
                    .supertypes
                    .iter()
                    .filter_map(|supertype| supertype.obj_internal()),
            )
            .collect::<Vec<_>>()
            .into_iter()
            .any(|supertype| self.is_ktype_subtype(supertype, seen))
    }

    fn type_description(&self, ty: Ty) -> Result<TypeDescription, Unsupported> {
        let (inner, nullable) = match ty.kind() {
            TyKind::Nullable(inner) => (inner, true),
            _ => (ty, false),
        };
        let (classifier, arguments) = match inner.kind() {
            TyKind::Obj(name, arguments) => {
                if let Some(class) = self.ir.class_id_by_name(name) {
                    let class = &self.ir.classes[class as usize];
                    if class.is_local_class || class.is_anonymous_object {
                        return Err(format!(
                            "`typeOf` of `{}`, whose class has no qualified name",
                            describe(ty)
                        ));
                    }
                }
                let arguments = arguments
                    .iter()
                    .map(|argument| self.type_projection(*argument))
                    .collect::<Result<Vec<_>, _>>()?;
                (self.described_classifier(name), arguments)
            }
            TyKind::Unit => (Classifier::Declared(krusty::types::wk::unit()), Vec::new()),
            TyKind::Nothing => (
                Classifier::Declared(krusty::types::wk::nothing()),
                Vec::new(),
            ),
            TyKind::Fun(signature) => {
                if signature.suspend {
                    return Err(format!(
                        "`typeOf` of the suspend function type `{}`",
                        describe(ty)
                    ));
                }
                let arguments = signature
                    .params
                    .iter()
                    .chain(std::iter::once(&signature.ret))
                    .map(|argument| self.type_description(*argument).map(Projection::Invariant))
                    .collect::<Result<Vec<_>, _>>()?;
                let arity = u8::try_from(signature.params.len())
                    .map_err(|_| format!("`typeOf` of the function type `{}`", describe(ty)))?;
                (Classifier::Function(arity), arguments)
            }
            _ => return Err(format!("`typeOf` of `{}`", describe(ty))),
        };
        Ok(TypeDescription {
            classifier,
            arguments,
            nullable,
        })
    }

    /// The classifier `name` describes: a `FunctionN` by the arity its provider published for it,
    /// so `Function1<A, B>` and `(A) -> B` describe one classifier.
    fn described_classifier(&self, name: krusty::types::TypeName) -> Classifier {
        match self.classifiers.classifier(name).and_then(|fact| fact.role) {
            Some(krusty::types::ClassifierRole::FunctionOfArity(arity)) => {
                Classifier::Function(arity)
            }
            _ => Classifier::Declared(name),
        }
    }

    fn type_projection(&self, argument: Ty) -> Result<Projection, Unsupported> {
        Ok(match argument.kind() {
            TyKind::StarProjection(_) => Projection::Star,
            TyKind::InProjection(argument) => Projection::In(self.type_description(argument)?),
            TyKind::OutProjection(argument) => Projection::Out(self.type_description(argument)?),
            _ => Projection::Invariant(self.type_description(argument)?),
        })
    }
}
