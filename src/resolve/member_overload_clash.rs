//! Conflicting member overloads, by Kotlin declaration identity.
//!
//! The checker compares published signatures: Kotlin name, extension receiver, and value
//! parameters, including generic arguments, nullability, and the `vararg` flag. A function's own
//! type parameters are alpha-equivalent by ordinal, and their declaration-owned count is part of
//! the identity even when a formal does not occur in the signature. The return type is not part
//! of that identity.
//! A missing or erroneous signature is left unresolved. Physical JVM descriptor collisions are a
//! backend concern, decided after representation.

use super::*;

#[derive(Clone, PartialEq, Eq, Hash)]
struct MemberParameter {
    vararg: bool,
    ty: Ty,
}

/// Kotlin declaration identity of one member function. This is not a JVM descriptor.
#[derive(Clone, PartialEq, Eq, Hash)]
struct MemberOverloadKey {
    name: String,
    receiver: Option<Ty>,
    parameters: Vec<MemberParameter>,
    /// Every formal the declaration owns, including ones that never occur in the signature.
    formal_count: u32,
    formal_bounds: Vec<(u32, Vec<Ty>)>,
}

impl Checker<'_> {
    pub(super) fn check_conflicting_member_overloads(
        &mut self,
        class_decl: DeclId,
        owner: Option<TypeName>,
        funs: &[&FunDecl],
    ) {
        let mut order = Vec::new();
        let mut groups: HashMap<MemberOverloadKey, Vec<Span>> = HashMap::new();
        for (index, function) in funs.iter().enumerate() {
            let Some(declaration) = self.member_declaration(class_decl, owner, index) else {
                continue;
            };
            if self.resolved_index.is_some_and(|index| {
                index
                    .declaration_applied_annotations(declaration)
                    .iter()
                    .any(crate::types::ResolvedAnnotation::is_deprecated_hidden)
            }) {
                continue;
            }
            let Some(key) = self.member_overload_key(declaration) else {
                continue;
            };
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(function.span);
        }
        let mut reported = Vec::new();
        for key in order {
            let Some(spans) = groups.get(&key) else {
                continue;
            };
            if spans.len() < 2 {
                continue;
            }
            reported.extend(spans.iter().copied());
        }
        reported.sort_by_key(|span| (span.lo, span.hi));
        for span in reported {
            self.diags.error(span, "conflicting overloads:");
        }
    }

    /// Join a class method to its stable declaration. The sibling ordinal is the same coordinate
    /// the surrounding member walk uses; it locates the declaration and is not a type.
    pub(super) fn member_declaration(
        &self,
        class_decl: DeclId,
        owner: Option<TypeName>,
        method_index: usize,
    ) -> Option<crate::fir::DeclarationId> {
        let method_index = u32::try_from(method_index).ok()?;
        let source_member = crate::libraries::SourceMember::Class {
            file: self.file_index,
            owner: class_decl.0,
            method: method_index,
        };
        // The active file has one parser→stable binding. Anywhere else the stable owner inventory
        // is the mapping. A miss in the mapping this phase owns stays a miss.
        if self.active_declarations.is_some() {
            return self.active_source_member_declaration(source_member);
        }
        let index = self.resolved_index?;
        let classifier = owner.and_then(|owner| index.classifier_declaration(owner))?;
        index.owned_declaration(
            classifier,
            crate::fir::DeclarationKind::Function,
            method_index,
        )
    }

    fn member_overload_key(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<MemberOverloadKey> {
        let index = self.resolved_index?;
        let signature = index.signature(declaration)?;
        let callable = index.callable_for_declaration(declaration)?;
        let name = index.callable_name(callable.id)?.to_string();
        let mut formals = Vec::new();
        let mut ordinal = 0u32;
        while let Some(parameter) = index.type_parameter(declaration, ordinal) {
            formals.push(index.type_parameter_semantic_name(parameter)?.to_string());
            ordinal += 1;
        }
        let formal_count = u32::try_from(formals.len()).ok()?;
        let normalize = |ty: Ty| crate::types::ty_canonicalize_params(ty, &formals);
        let raw_receiver = callable
            .shape
            .extension_receiver
            .map(|receiver| receiver.get());
        if raw_receiver
            .is_some_and(|receiver| receiver.contains_error() || receiver.mentions_pending())
        {
            return None;
        }
        let mut raw_parameters = Vec::with_capacity(signature.parameters.len());
        let mut parameters = Vec::with_capacity(signature.parameters.len());
        for (ordinal, parameter) in signature.parameters.iter().enumerate() {
            let ty = parameter.get();
            if ty.contains_error() || ty.mentions_pending() {
                return None;
            }
            let ordinal = u32::try_from(ordinal).ok()?;
            let vararg = index
                .callable_parameter(callable.id, ordinal)?
                .flags()
                .is_vararg();
            raw_parameters.push(ty);
            parameters.push(MemberParameter {
                vararg,
                ty: normalize(ty),
            });
        }
        let mentions =
            |ty: Ty, formal: &String| ty_mentions_param(ty, std::slice::from_ref(formal));
        let mut formal_bounds = Vec::new();
        for (ordinal, formal) in formals.iter().enumerate() {
            let mentioned = raw_parameters
                .iter()
                .copied()
                .chain(raw_receiver)
                .any(|ty| mentions(ty, formal));
            if !mentioned {
                continue;
            }
            let parameter = index.type_parameter(declaration, ordinal as u32)?;
            let header = index.type_parameter_header(parameter)?;
            formal_bounds.push((
                ordinal as u32,
                header
                    .bounds
                    .iter()
                    .map(|bound| normalize(bound.ty.get()))
                    .collect(),
            ));
        }
        let receiver = raw_receiver.map(normalize);
        Some(MemberOverloadKey {
            name,
            receiver,
            parameters,
            formal_count,
            formal_bounds,
        })
    }
}
