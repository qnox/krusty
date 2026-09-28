//! kotlinc's diagnostic wording for a declaration-owned type parameter: `T (of fun <T : B> f)` or
//! `X (of class Box<out X, in Y : Number>)`. The owner text lists every formal of the declaration
//! with its variance and declared bounds (an explicit `Any?` bound is the default and is omitted,
//! and `reified` is not part of the wording). Owners are keyed by the formal's semantic identity,
//! which is unique per declaration. A function's formals are named with their owner only while that
//! function is checked; a formal leaking out through an unsolved call keeps its bare name.

use super::{Checker, CheckerScope};
use crate::ast::{ClassDecl, ClassKind, FunDecl, TypeRef};
use crate::types::{Ty, TypeVariance};

impl Checker<'_> {
    /// Record the owner wording of a function's formals, bound in `scope`, returning their
    /// identities for [`Self::retire_type_parameter_owners`] when the function's check ends.
    pub(super) fn publish_function_type_parameter_owner(
        &mut self,
        scope: &CheckerScope<'_>,
        function: &FunDecl,
    ) -> Vec<&'static str> {
        if function.type_params.is_empty() {
            return Vec::new();
        }
        let formals = self.type_parameter_owner_formals(
            scope,
            &function.type_params,
            &function.type_param_bounds,
            &[],
        );
        self.publish_type_parameter_owner(
            scope,
            &function.type_params,
            format!("fun <{formals}> {}", function.name),
        )
    }

    pub(super) fn retire_type_parameter_owners(&mut self, parameters: &[&'static str]) {
        for parameter in parameters {
            self.type_parameter_owners.remove(parameter);
        }
    }

    /// Record the owner wording of a classifier's formals, bound in `scope`.
    pub(super) fn publish_class_type_parameter_owner(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
    ) {
        if class.type_params.is_empty() {
            return;
        }
        let keyword = match class.kind {
            ClassKind::Interface => "interface",
            ClassKind::Class | ClassKind::Enum | ClassKind::Annotation => "class",
        };
        let formals = self.type_parameter_owner_formals(
            scope,
            &class.type_params,
            class.type_param_bounds(),
            class.type_param_variances(),
        );
        self.publish_type_parameter_owner(
            scope,
            &class.type_params,
            format!("{keyword} {}<{formals}>", class.name),
        );
    }

    /// `ty` as a diagnostic names it: a declaration-owned type parameter with its owner
    /// (`T (of fun <T : B> f)`), and classifiers qualified only where `context` makes them clash.
    pub(super) fn diagnostic_type_name(&self, ty: Ty, context: &[Ty]) -> String {
        ty.source_name_with_type_parameter_in(context, &|parameter| {
            let source = crate::types::type_parameter_source_name(parameter);
            match self.type_parameter_owners.get(parameter) {
                Some(owner) => format!("{source} (of {owner})"),
                None => source.to_string(),
            }
        })
    }

    fn publish_type_parameter_owner(
        &mut self,
        scope: &CheckerScope<'_>,
        names: &[String],
        owner: String,
    ) -> Vec<&'static str> {
        let identities = names
            .iter()
            .filter_map(|name| scope.tparam_bound(name).ty_param_name())
            .collect::<Vec<_>>();
        for identity in &identities {
            self.type_parameter_owners.insert(identity, owner.clone());
        }
        identities
    }

    fn type_parameter_owner_formals(
        &self,
        scope: &CheckerScope<'_>,
        names: &[String],
        declared_bounds: &[(String, TypeRef)],
        variances: &[TypeVariance],
    ) -> String {
        names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let variance = match variances.get(index) {
                    Some(TypeVariance::In) => "in ",
                    Some(TypeVariance::Out) => "out ",
                    Some(TypeVariance::Invariant) | None => "",
                };
                // Only the bounds written on this parameter: the lexical binding also carries the
                // ones inherited through a bare parameter bound (`T : U`), after the written ones.
                let written = declared_bounds
                    .iter()
                    .filter(|(owner, _)| owner == name)
                    .count();
                let parameter = scope.tparam_bound(name);
                let bounds = parameter
                    .ty_param_bound()
                    .into_iter()
                    .chain(self.semantic_tparam_extra_bounds(scope, parameter))
                    .take(written)
                    .filter(|bound| *bound != Ty::nullable(Ty::obj("kotlin/Any")))
                    .map(Ty::source_name)
                    .collect::<Vec<_>>();
                if bounds.is_empty() {
                    format!("{variance}{name}")
                } else {
                    format!("{variance}{name} : {}", bounds.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}
