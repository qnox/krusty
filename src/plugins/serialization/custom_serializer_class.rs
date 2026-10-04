//! kotlinx.serialization's contract for a custom serializer CLASS that
//! `@Serializable(with = X::class)` names.
//!
//! kotlinc never constructs such a serializer at a use site. The served classifier's generated
//! `serializer(typeSerial0, …)` accessor does, through `X`'s PRIMARY constructor, which must take
//! either no parameters or exactly one `KSerializer` per type parameter of the served classifier.
//! The accessor passes its `typeSerialN` operands to those parameters positionally; the order of
//! `X`'s own type parameters plays no part. Any other primary constructor is rejected by the
//! plugin's FIR checker at the `@Serializable` annotation, with one diagnostic for a wrong
//! parameter count and one for each parameter that is not a `KSerializer`.

use crate::plugins::{
    FrontendNamedClass, FrontendNamedClassConstruction, FrontendNamedClassContext,
    FrontendPluginDiagnostic,
};
use crate::types::{type_name, AnnotationValue, Ty};

use super::{PluginRelease, KSERIALIZER_FQ, SERIALIZABLE_FQ};

/// Select the primary constructor of the serializer class `ctx`'s `@Serializable(with = …)` names,
/// or report why that constructor breaks the contract. A `with =` object, or a class this
/// compilation does not declare, is not a named source class and selects nothing.
pub(super) fn select_constructor(
    ctx: &FrontendNamedClassContext<'_>,
    release: Option<PluginRelease>,
    diagnostics: &mut Vec<FrontendPluginDiagnostic>,
) -> Option<FrontendNamedClassConstruction> {
    let serializable = type_name(SERIALIZABLE_FQ);
    let custom = ctx
        .applied_annotations
        .iter()
        .find(|application| application.annotation == serializable)?
        .arguments
        .iter()
        .find_map(|(name, value)| match value {
            AnnotationValue::Class(serializer) if name == "with" => {
                serializer.kotlin_class_internal()
            }
            _ => None,
        })?;
    let serializer = ctx
        .named_classes
        .iter()
        .find(|class| class.classifier == custom)?;
    let parameters = &serializer.primary_constructor;
    if let Some(operands) = constructor_operands(ctx.type_parameter_count, parameters) {
        return Some(FrontendNamedClassConstruction {
            classifier: custom,
            operands,
        });
    }
    let span = ctx
        .annotations
        .iter()
        .find(|(annotation, _)| *annotation == serializable)
        .map(|(_, span)| *span)?;
    let served = served_display(serializer)?;
    diagnostics.extend(
        contract_violations(
            ctx.type_parameter_count,
            &serializer.display_name,
            served,
            parameters,
            release,
        )
        .into_iter()
        .map(|message| FrontendPluginDiagnostic { span, message }),
    );
    None
}

/// The type the serializer class serializes, as diagnostics render it: the argument of its
/// `KSerializer<…>` supertype.
fn served_display(serializer: &FrontendNamedClass) -> Option<&str> {
    let kserializer = type_name(KSERIALIZER_FQ);
    serializer
        .supertype_arguments
        .iter()
        .find(|(supertype, _)| *supertype == kserializer)?
        .1
        .first()
        .map(String::as_str)
}

/// The accessor's operand ordinal passed to each primary-constructor parameter, in parameter
/// order, or `None` when the constructor breaks the contract.
fn constructor_operands(type_parameters: usize, parameters: &[(String, Ty)]) -> Option<Box<[u32]>> {
    if parameters.is_empty() {
        return Some(Box::new([]));
    }
    (parameters.len() == type_parameters && parameters.iter().all(|(_, ty)| is_serializer(*ty)))
        .then(|| (0..type_parameters as u32).collect())
}

/// kotlinc's diagnostics for a primary constructor that breaks the contract, in its order: the
/// parameter count first, then each non-`KSerializer` parameter. The wording follows the plugin
/// release; without a known release it is the newest.
fn contract_violations(
    type_parameters: usize,
    display_name: &str,
    served: &str,
    parameters: &[(String, Ty)],
    release: Option<PluginRelease>,
) -> Vec<String> {
    let newest = release.is_none_or(|release| release >= PluginRelease::V2_4_20);
    let cannot = if newest { "cannot" } else { "can not" };
    let prefix = format!("custom serializer '{display_name}' {cannot} be used for '{served}'");
    let count = parameters.len();
    let mut violations = Vec::new();
    if count != 0 && count != type_parameters {
        let constructor = if newest {
            "the primary constructor"
        } else {
            "primary constructor"
        };
        let expected = if type_parameters == 0 {
            "expected no parameters but".to_string()
        } else {
            format!("expected no parameters or {type_parameters}, but")
        };
        violations.push(format!(
            "{prefix} since it has an invalid number of parameters in {constructor}: {expected} \
             has {count} parameters"
        ));
    }
    violations.extend(
        parameters
            .iter()
            .filter(|(_, ty)| !is_serializer(*ty))
            .map(|(name, _)| {
                if newest {
                    format!(
                        "{prefix}. The type of parameter '{name}' in the serializer's primary \
                         constructor must be 'KSerializer'."
                    )
                } else {
                    format!(
                        "{prefix}, type of parameter '{name}' in serializers primary constructor \
                         should be 'KSerializer'"
                    )
                }
            }),
    );
    violations
}

/// Whether `ty` is the `KSerializer` interface itself, the parameter type the contract requires.
fn is_serializer(ty: Ty) -> bool {
    ty.non_null().kotlin_class_internal() == Some(type_name(KSERIALIZER_FQ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serializer_of(argument: Ty) -> Ty {
        Ty::obj_args(KSERIALIZER_FQ, &[argument])
    }

    fn parameter(name: &str, ty: Ty) -> (String, Ty) {
        (name.to_string(), ty)
    }

    #[test]
    fn operands_follow_the_parameter_positions() {
        let first = Ty::ty_param("A", Ty::nullable(Ty::obj("kotlin/Any")));
        let second = Ty::ty_param("B", Ty::nullable(Ty::obj("kotlin/Any")));
        let parameters = [
            parameter("second", serializer_of(second)),
            parameter("first", serializer_of(first)),
        ];
        assert_eq!(
            constructor_operands(2, &parameters).as_deref(),
            Some([0, 1].as_slice())
        );
        assert_eq!(constructor_operands(2, &[]).as_deref(), Some([].as_slice()));
        assert_eq!(constructor_operands(1, &parameters), None);
    }

    #[test]
    fn a_broken_constructor_reports_count_then_types_in_each_release_wording() {
        let parameters = [
            parameter("a", serializer_of(Ty::String)),
            parameter("b", Ty::String),
        ];
        assert_eq!(
            contract_violations(
                1,
                "S<*>",
                "C<T (of class S<T>)>",
                &parameters,
                Some(PluginRelease::new(2, 4, 10))
            ),
            vec![
                "custom serializer 'S<*>' can not be used for 'C<T (of class S<T>)>' since it has \
                 an invalid number of parameters in primary constructor: expected no parameters \
                 or 1, but has 2 parameters"
                    .to_string(),
                "custom serializer 'S<*>' can not be used for 'C<T (of class S<T>)>', type of \
                 parameter 'b' in serializers primary constructor should be 'KSerializer'"
                    .to_string(),
            ]
        );
        assert_eq!(
            contract_violations(1, "S<*>", "C<T (of class S<T>)>", &parameters, None),
            vec![
                "custom serializer 'S<*>' cannot be used for 'C<T (of class S<T>)>' since it has \
                 an invalid number of parameters in the primary constructor: expected no \
                 parameters or 1, but has 2 parameters"
                    .to_string(),
                "custom serializer 'S<*>' cannot be used for 'C<T (of class S<T>)>'. The type of \
                 parameter 'b' in the serializer's primary constructor must be 'KSerializer'."
                    .to_string(),
            ]
        );
    }

    #[test]
    fn a_non_generic_class_expects_no_parameters() {
        let parameters = [parameter("inner", serializer_of(Ty::String))];
        assert_eq!(
            contract_violations(0, "S", "C", &parameters, None),
            vec![
                "custom serializer 'S' cannot be used for 'C' since it has an invalid number of \
                 parameters in the primary constructor: expected no parameters but has 1 \
                 parameters"
                    .to_string()
            ]
        );
    }
}
