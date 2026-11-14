//! A Java method that overrides a type-safe Kotlin collection declaration.
//!
//! `java.util.Deque` redeclares `contains(Object)` and `remove(Object)`, and `java.util.HashMap`
//! declares `get(Object)`. Each is the JVM realization of a Kotlin collection declaration with a
//! special generic signature (`Collection.contains(element: E)`, `Map.get(key: K)`), whose
//! parameters Java erases to `Object`. kotlinc's Java scope reads such a method as an override of
//! that declaration and gives it the declaration's parameter types, substituted through the Java
//! class's own supertypes (`Deque<E>.contains(element: E)`). The provider applies the same rule
//! while the method is still a classfile member, so core resolution sees one Kotlin declaration
//! shape whichever classifier, Java or Kotlin, declared it.

use super::java_nullability::java_type_nullability;
use super::{parse_class_gsig, JvmLibraries};
use crate::jvm::classreader::ClassInfo;
use crate::jvm::mapped_builtin_declarations::{
    collection_bridge_policy_for_declaration, MappedBuiltinMemberKind,
};
use crate::jvm::JavaNullability;
use crate::symbol_resolver::ty_subst;
use crate::types::{Ty, TypeName};

/// One Java method's classfile identity, with the declared nullability of its parameters.
pub(super) struct JavaMethod<'a> {
    pub(super) name: &'a str,
    pub(super) descriptor: &'a str,
    pub(super) parameter_nullability: &'a [Option<JavaNullability>],
}

impl JvmLibraries {
    /// The Kotlin parameter types of the type-safe collection declaration `method` overrides, as
    /// `class` applies it, or `None` when `method` overrides no such declaration.
    ///
    /// A raw supertype (`implements Deque`) applies no arguments; the method then keeps its erased
    /// Java parameters, which is what the raw supertype's substitution would give.
    pub(super) fn type_safe_builtin_override_params(
        &self,
        class: &ClassInfo,
        class_bounds: &crate::fx_hash::HashMap<String, Ty>,
        method: JavaMethod<'_>,
    ) -> Option<Vec<Ty>> {
        let (declaration_owner, parameters) =
            self.type_safe_builtin_declaration(class.this_class, method.name, method.descriptor)?;
        let formals = class
            .signature
            .as_deref()
            .and_then(parse_class_gsig)
            .map(|(formals, _, _)| formals)
            .unwrap_or_default();
        let own_arguments = formals
            .iter()
            .map(|formal| Some(Ty::ty_param(formal, *class_bounds.get(formal)?)))
            .collect::<Option<Vec<_>>>()?;
        let (_, arguments) = self.receiver_type_application_name(
            Ty::obj_args_name(class.this_class, &own_arguments),
            declaration_owner,
        )?;
        let declaration_formals = self
            .cp
            .builtin_class_signature_name(declaration_owner)?
            .formals;
        if arguments.len() != declaration_formals.len() {
            return None;
        }
        let bindings: crate::fx_hash::HashMap<String, Ty> =
            declaration_formals.into_iter().zip(arguments).collect();
        Some(
            parameters
                .into_iter()
                .enumerate()
                .map(|(index, parameter)| {
                    java_type_nullability(
                        ty_subst(parameter, &bindings),
                        method.parameter_nullability.get(index).copied().flatten(),
                    )
                })
                .collect(),
        )
    }

    /// The Kotlin collection declaration with a special generic signature that a method named
    /// `name` with JVM `descriptor` realizes in `class`'s hierarchy, and its declared parameters.
    /// The Java hierarchy only names the mapped collection interfaces; the declarations and their
    /// Kotlin inheritance come from `.kotlin_builtins`.
    fn type_safe_builtin_declaration(
        &self,
        class: TypeName,
        name: &str,
        descriptor: &str,
    ) -> Option<(TypeName, Vec<Ty>)> {
        let mut seen = crate::fx_hash::HashSet::default();
        let mut pending = std::collections::VecDeque::from([class]);
        while let Some(owner) = pending.pop_front() {
            if !seen.insert(owner) {
                continue;
            }
            if let Some(face) =
                crate::jvm::jvm_class_map::jvm_collection_to_kotlin_mutable_type_name(owner)
            {
                if let Some(declaration) =
                    self.builtin_type_safe_declaration(face, name, descriptor)
                {
                    return Some(declaration);
                }
            }
            if let Some(class) = self.cp.find_name(owner) {
                pending.extend(class.interfaces.iter_ids().chain(class.super_class));
            }
        }
        None
    }

    /// The nearest declaration in `face`'s builtins hierarchy that carries a type-safe policy row.
    fn builtin_type_safe_declaration(
        &self,
        face: TypeName,
        name: &str,
        descriptor: &str,
    ) -> Option<(TypeName, Vec<Ty>)> {
        let mut seen = crate::fx_hash::HashSet::default();
        let mut pending = std::collections::VecDeque::from([face]);
        while let Some(current) = pending.pop_front() {
            if !seen.insert(current) {
                continue;
            }
            let declaration = self
                .cp
                .decoded_builtin_members_name(current)
                .into_iter()
                .find(|member| {
                    member.kind == MappedBuiltinMemberKind::Function
                        && member.source_name == name
                        && member.physical_name == name
                        && member.descriptor == descriptor
                });
            if let Some(declaration) = declaration.filter(|_| {
                collection_bridge_policy_for_declaration(
                    current,
                    name,
                    descriptor,
                    MappedBuiltinMemberKind::Function,
                )
                .is_some()
            }) {
                return Some((current, declaration.generic_sig.params));
            }
            pending.extend(self.cp.builtin_supertypes_name(current).iter_ids());
        }
        None
    }
}
