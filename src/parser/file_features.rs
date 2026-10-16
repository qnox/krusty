//! Language-policy bits a parsed file carries into later phases.

use crate::ast::File;
use crate::features::LangFeatures;

/// Install the semantic language-policy bits consumed after parsing. Full-file parsing and bounded
/// declaration-unit reparsing must project the feature set through this one operation; otherwise a
/// Pass-2 unit can resolve the same source under different language rules than Pass 1.
pub(super) fn apply_file_features(file: &mut File, features: &LangFeatures) {
    file.assert_always_enabled = features.has("AssertionsAlwaysEnable");
    file.assert_always_disabled = features.has("AssertionsAlwaysDisable");
    file.explicit_context_arguments = features.has("ExplicitContextArguments");
    file.context_sensitive_resolution_using_expected_type =
        features.has("ContextSensitiveResolutionUsingExpectedType");
    file.allow_protected_super_companion_property_access =
        features.has("AllowAccessToProtectedFieldFromSuperCompanion");
    file.bare_array_class_literal = features.has("BareArrayClassLiteral");
    file.forbid_array_of_nothing_in_lhs_of_class_literal =
        features.has("ForbidArrayOfNothingInLhsOfClassLiteral");
    file.enum_entries_enabled = features.has("EnumEntries");
    file.prioritized_enum_entries = features.has("PrioritizedEnumEntries");
    file.implicit_signed_to_unsigned_integer_conversion =
        features.has("ImplicitSignedToUnsignedIntegerConversion");
    file.data_copy_respects_ctor_visibility =
        features.has("DataClassCopyRespectsConstructorVisibility");
    file.unit_conversions_on_arbitrary_expressions =
        features.has("UnitConversionsOnArbitraryExpressions");
    file.eager_lambda_analysis = features.has("EagerLambdaAnalysis");
    file.allow_eager_supertype_accessibility_checks =
        features.has("AllowEagerSupertypeAccessibilityChecks");
    file.lambda_implementation_uses_inferred_result =
        features.language_version() < crate::language_version::LanguageVersion::V2_4;
    file.full_value_classes = features.has("FullValueClasses");
    file.forbid_value_class_recursion_via_type_parameters =
        features.has("ForbidValueClassRecursionViaTypeParameters");
    file.value_class_recursion_via_type_parameters_warning = features.deprecation_warning_message(
        "ForbidValueClassRecursionViaTypeParameters",
        "value class cannot be recursive.",
    );
    file.forbid_operator_equals_in_enum_entries_and_anonymous_objects =
        features.has("ForbidOperatorEqualsInEnumEntriesAndAnonymousObjects");
    file.inapplicable_operator_modifier_warning = features.deprecation_warning_message(
        "ForbidOperatorEqualsInEnumEntriesAndAnonymousObjects",
        "'operator' modifier is not applicable to function: must override 'equals()' in Any.",
    );
    file.custom_equals_in_value_classes = features.has("CustomEqualsInValueClasses");
    file.forbid_useless_type_arguments = features.has("ForbidUselessTypeArgumentsIn25");
    file.type_arguments_not_allowed_warning = features.deprecation_warning_message(
        "ForbidUselessTypeArgumentsIn25",
        "type arguments are not allowed when static member is accessed.",
    );
    file.recursive_type_of = features.has("JvmSupportRecursiveTypeOf");
    file.opted_in_markers = features.opted_in().map(str::to_string).collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin_version::KotlinVersion;
    use crate::language_version::LanguageVersion;

    #[test]
    fn deprecation_policies_come_from_each_compilations_release() {
        let settings = |release| {
            LangFeatures::for_release(release, LanguageVersion::V2_4, LanguageVersion::V2_4)
        };
        let mut old = File::default();
        apply_file_features(&mut old, &settings(KotlinVersion::V2_4_0));
        let mut patch = File::default();
        apply_file_features(&mut patch, &settings(KotlinVersion::V2_4_10));
        let mut current = File::default();
        apply_file_features(&mut current, &settings(KotlinVersion::V2_4_20));
        let type_arguments_not_allowed_warning = Some(
            "type arguments are not allowed when static member is accessed. This will become an \
             error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-390.",
        );

        assert_eq!(old.value_class_recursion_via_type_parameters_warning, None);
        assert_eq!(old.inapplicable_operator_modifier_warning, None);
        assert_eq!(
            old.type_arguments_not_allowed_warning.as_deref(),
            type_arguments_not_allowed_warning
        );
        assert_eq!(
            patch.type_arguments_not_allowed_warning.as_deref(),
            type_arguments_not_allowed_warning
        );
        assert_eq!(
            current.value_class_recursion_via_type_parameters_warning,
            Some(
                "value class cannot be recursive. This will become an error in language version \
                 2.5. See https://youtrack.jetbrains.com/issue/KT-85848."
                    .to_string()
            )
        );
        assert_eq!(
            current.inapplicable_operator_modifier_warning,
            Some(
                "'operator' modifier is not applicable to function: must override 'equals()' in \
                 Any. This will become an error in language version 2.5. See \
                 https://youtrack.jetbrains.com/issue/KT-86143."
                    .to_string()
            )
        );
        assert_eq!(
            current.type_arguments_not_allowed_warning.as_deref(),
            type_arguments_not_allowed_warning
        );
    }
}
