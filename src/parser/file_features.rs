//! Language-policy bits a parsed file carries into later phases.

use crate::ast::File;
use crate::features::LangFeatures;

/// Install the semantic language-policy bits consumed after parsing. Full-file parsing and bounded
/// declaration-unit reparsing must project the feature set through this one operation; otherwise a
/// Pass-2 unit can resolve the same source under different language rules than Pass 1.
pub(super) fn apply_file_features(file: &mut File, features: &LangFeatures) {
    file.assertions = features.assertions();
    file.language.explicit_context_arguments = features.has("ExplicitContextArguments");
    file.language.callable_references_to_contextual =
        features.has("CallableReferencesToContextual");
    file.language
        .context_sensitive_resolution_using_expected_type =
        features.has("ContextSensitiveResolutionUsingExpectedType");
    file.language
        .allow_protected_super_companion_property_access =
        features.has("AllowAccessToProtectedFieldFromSuperCompanion");
    file.language
        .enforce_missing_named_arguments_on_java_annotation =
        features.has("EnforceMissingNamedArgumentsOnJavaAnnotation");
    file.language
        .positional_java_annotation_call_argument_warning = features
        .labelled_deprecation_warning_message(
            "EnforceMissingNamedArgumentsOnJavaAnnotation",
            "only named arguments are available for Java annotations.",
        );
    file.language.bare_array_class_literal = features.has("BareArrayClassLiteral");
    file.language
        .forbid_array_of_nothing_in_lhs_of_class_literal =
        features.has("ForbidArrayOfNothingInLhsOfClassLiteral");
    file.language.enum_entries_enabled = features.has("EnumEntries");
    file.language.collection_literals_enabled = features.has("CollectionLiterals");
    file.language.prioritized_enum_entries = features.has("PrioritizedEnumEntries");
    file.language.implicit_signed_to_unsigned_integer_conversion =
        features.has("ImplicitSignedToUnsignedIntegerConversion");
    file.language.data_copy_respects_ctor_visibility =
        features.has("DataClassCopyRespectsConstructorVisibility");
    file.language.property_annotation_default_target_mode =
        if features.has("PropertyParamAnnotationDefaultTargetMode") {
            crate::types::PropertyAnnotationDefaultTargetMode::ParamProperty
        } else {
            crate::types::PropertyAnnotationDefaultTargetMode::FirstOnly
        };
    file.language.unit_conversions_on_arbitrary_expressions =
        features.has("UnitConversionsOnArbitraryExpressions");
    file.language.eager_lambda_analysis = features.has("EagerLambdaAnalysis");
    file.language.allow_eager_supertype_accessibility_checks =
        features.has("AllowEagerSupertypeAccessibilityChecks");
    file.language.sam_constructor_missing_dependency = features
        .table()
        .get("ForbidSamConstructorCallsWithMissingDependencySupertype")
        .map(|_| features.has("ForbidSamConstructorCallsWithMissingDependencySupertype"));
    file.language.lambda_implementation_uses_inferred_result =
        features.language_version() < crate::language_version::LanguageVersion::V2_4;
    file.language.full_value_classes = features.has("FullValueClasses");
    file.language.explicit_inline_value_classes = features.has("ExplicitInlineValueClassesSyntax");
    file.language.smart_cast_value_class_components =
        features.has("AllowSmartCastsOnValueClassUnderlyingProperties");
    file.language
        .forbid_value_class_recursion_via_type_parameters =
        features.has("ForbidValueClassRecursionViaTypeParameters");
    file.language
        .value_class_recursion_via_type_parameters_warning = features.deprecation_warning_message(
        "ForbidValueClassRecursionViaTypeParameters",
        "Value class cannot be recursive.",
    );
    file.language.deprecation_migrations =
        crate::features::DeprecationMigrations::from_features(features);
    file.language
        .forbid_operator_equals_in_enum_entries_and_anonymous_objects =
        features.has("ForbidOperatorEqualsInEnumEntriesAndAnonymousObjects");
    file.language.inapplicable_operator_modifier_warning = features.deprecation_warning_message(
        "ForbidOperatorEqualsInEnumEntriesAndAnonymousObjects",
        "'operator' modifier is not applicable to function: must override 'equals()' in Any.",
    );
    file.language.custom_equals_in_value_classes = features.has("CustomEqualsInValueClasses");
    file.language.normalize_nan_in_const_context = features.has("NormalizeNaNValuesInConstContext");
    file.language.no_when_branch_matched_with_subject =
        features.has("NoWhenBranchMatchedExceptionWithMessage");
    file.language.intrinsic_const_evaluation = features.has("IntrinsicConstEvaluation");
    // kotlinc binds the generated structural `equals` under `StrictEquals` from the release that
    // introduced strict equality for structural classes; 2.4 leaves it unbound.
    file.language.structural_equality_bound = features.has("StrictEquals")
        && features
            .table()
            .get("StrictEqualsForStructuralClasses")
            .is_some();
    file.language.interface_companion_block_members_are_static =
        features.table().get("CompanionBlocks").is_some();
    file.language
        .forbid_upper_bounds_violation_on_type_operator_and_parameter_bounds =
        features.has("ForbidUpperBoundsViolationOnTypeOperatorAndParameterBounds");
    // 2.5.0-RC renamed the feature and moved its since-version from 2.5 to 2.6. A release
    // declares one spelling; the bit is that spelling.
    let useless_type_arguments = [
        "ForbidUselessTypeArgumentsIn26",
        "ForbidUselessTypeArgumentsIn25",
    ]
    .into_iter()
    .find(|name| features.table().get(name).is_some())
    .unwrap_or("ForbidUselessTypeArgumentsIn25");
    file.language.forbid_useless_type_arguments = features.has(useless_type_arguments);
    file.language.useless_type_arguments_warning_format =
        features.deprecation_warning_format(useless_type_arguments);
    file.language.forbid_package_qualifier_forms =
        features.has("ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier");
    file.language.parenthesized_package_qualifier_warning = features.deprecation_warning_message(
        "ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier",
        crate::features::deprecation_cycle_messages::PARENTHESIZED_PACKAGE_QUALIFIER,
    );
    file.language.package_qualifier_type_arguments_warning = features.deprecation_warning_message(
        "ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier",
        crate::features::deprecation_cycle_messages::PACKAGE_QUALIFIER_TYPE_ARGUMENTS,
    );
    file.language.type_arguments_not_allowed_warning = features.deprecation_warning_message(
        useless_type_arguments,
        "Type arguments are not allowed when static member is accessed.",
    );
    file.language
        .explicit_type_arguments_in_property_access_warning = features.deprecation_warning_message(
        useless_type_arguments,
        "Object access cannot have explicit type arguments.",
    );
    file.language.forbid_java_class_property_references =
        features.has("ForbidJavaClassPropertyReferences");
    file.language.java_class_property_reference_warning = features.labelled_deprecation_warning_message(
        "ForbidJavaClassPropertyReferences",
        "'::javaClass' returns a property reference and is error-prone. Use '.javaClass' or '::class.java' to access the Java class of the receiver.",
    );
    file.language.error_about_data_class_copy_visibility_change =
        features.has("ErrorAboutDataClassCopyVisibilityChange");
    file.language.data_class_copy_visibility_change_warning = features.deprecation_warning_message(
        "ErrorAboutDataClassCopyVisibilityChange",
        crate::features::deprecation_cycle_messages::DATA_CLASS_COPY_VISIBILITY,
    );
    file.language.data_class_invisible_copy_usage_warning = features.deprecation_warning_message(
        "ErrorAboutDataClassCopyVisibilityChange",
        crate::features::deprecation_cycle_messages::DATA_CLASS_INVISIBLE_COPY_USAGE,
    );
    file.language
        .report_type_variance_conflicts_in_dnn_and_flexible =
        features.has("ReportTypeVarianceConflictsInDnnAndFlexible");
    file.language.reified_type_arguments = crate::features::ReifiedTypeArgumentFeatures {
        report_in_dnn_and_flexible: features.has("ReportReificationProblemsInDnnAndFlexible"),
        report_in_dnn_and_flexible_warning_format: features
            .deprecation_warning_format("ReportReificationProblemsInDnnAndFlexible"),
        prohibit_intersection: features.has("ProhibitIntersectionReifiedTypeParameter"),
        prohibit_intersection_warning_format: features
            .deprecation_warning_format("ProhibitIntersectionReifiedTypeParameter"),
        nullable_nothing: features.has("NullableNothingInReifiedPosition"),
    };
    file.language.recursive_type_of = features.has("JvmSupportRecursiveTypeOf");
    file.language.opted_in_markers = features.opted_in().map(str::to_string).collect();
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
            "Type arguments are not allowed when static member is accessed. This will become an \
             error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-390.",
        );
        let property_access_warning = Some(
            "Object access cannot have explicit type arguments. This will become an error in \
             language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-390.",
        );
        let dynamic_warning = |file: &File| {
            file.language
                .useless_type_arguments_warning_format
                .as_ref()
                .map(|format| {
                    format.message("One type argument expected for 'typealias Box<T> = String'.")
                })
        };
        let expected_dynamic_warning = Some(
            "One type argument expected for 'typealias Box<T> = String'. This will become an \
             error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-390."
                .to_string(),
        );

        assert_eq!(
            old.language
                .value_class_recursion_via_type_parameters_warning,
            None
        );
        assert_eq!(old.language.parenthesized_package_qualifier_warning, None);
        assert_eq!(
            patch.language.package_qualifier_type_arguments_warning,
            None
        );
        assert_eq!(
            current
                .language
                .parenthesized_package_qualifier_warning
                .as_deref(),
            Some(
                "Wrapping package qualifiers in parentheses is not allowed. This will become an \
                 error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-396."
            )
        );
        assert_eq!(
            current
                .language
                .package_qualifier_type_arguments_warning
                .as_deref(),
            Some(
                "Type arguments are not allowed for packages. This will become an error in \
                 language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-396."
            )
        );
        assert_eq!(old.language.inapplicable_operator_modifier_warning, None);
        assert_eq!(
            old.language.data_class_copy_visibility_change_warning.as_deref(),
            Some(
                "Non-public primary constructor is exposed via the generated 'copy()' method of the 'data' class.\n\nThe generated 'copy()' will change its visibility in future releases.\n\nTo suppress the warning do one of the following:\n- Annotate the data class with the '@ConsistentCopyVisibility' annotation.\n- Use the '-Xconsistent-data-class-copy-visibility' compiler flag.\n- Annotate the data class with the '@ExposedCopyVisibility' annotation \n  (Discouraged, but can be used to keep binary compatibility).\n\nTo learn more, see the documentation of the '@ConsistentCopyVisibility' and '@ExposedCopyVisibility' annotations.\n\nThis will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KT-11914."
            )
        );
        assert_eq!(
            old.language.data_class_invisible_copy_usage_warning.as_deref(),
            Some(
                "This 'copy()' exposes the non-public primary constructor of a 'data class'. See the appropriate 'data class' documentation or contact the 'data class' author for migration guidance. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KT-11914."
            )
        );
        assert_eq!(
            old.language.type_arguments_not_allowed_warning.as_deref(),
            type_arguments_not_allowed_warning
        );
        assert_eq!(
            old.language
                .explicit_type_arguments_in_property_access_warning
                .as_deref(),
            property_access_warning
        );
        assert_eq!(dynamic_warning(&old), expected_dynamic_warning);
        assert_eq!(
            patch.language.type_arguments_not_allowed_warning.as_deref(),
            type_arguments_not_allowed_warning
        );
        assert_eq!(dynamic_warning(&patch), expected_dynamic_warning);
        assert_eq!(
            current
                .language
                .value_class_recursion_via_type_parameters_warning,
            Some(
                "Value class cannot be recursive. This will become an error in language version \
                 2.5. See https://youtrack.jetbrains.com/issue/KT-85848."
                    .to_string()
            )
        );
        assert_eq!(
            current.language.inapplicable_operator_modifier_warning,
            Some(
                "'operator' modifier is not applicable to function: must override 'equals()' in \
                 Any. This will become an error in language version 2.5. See \
                 https://youtrack.jetbrains.com/issue/KT-86143."
                    .to_string()
            )
        );
        assert_eq!(
            current
                .language
                .type_arguments_not_allowed_warning
                .as_deref(),
            type_arguments_not_allowed_warning
        );
        assert_eq!(
            current
                .language
                .explicit_type_arguments_in_property_access_warning
                .as_deref(),
            property_access_warning
        );
        assert_eq!(dynamic_warning(&current), expected_dynamic_warning);
    }

    #[test]
    fn rc_postpones_useless_type_arguments_under_the_renamed_feature() {
        let mut before = File::default();
        apply_file_features(
            &mut before,
            &LangFeatures::for_release(
                KotlinVersion::V2_5_0_RC,
                LanguageVersion::V2_4,
                LanguageVersion::V2_4,
            ),
        );
        assert!(!before.language.forbid_useless_type_arguments);
        assert_eq!(
            before
                .language
                .type_arguments_not_allowed_warning
                .as_deref(),
            Some(
                "Type arguments are not allowed when static member is accessed. This will become \
                 an error in language version 2.6. See \
                 https://youtrack.jetbrains.com/issue/KTLC-390."
            )
        );

        let mut at_since = File::default();
        apply_file_features(
            &mut at_since,
            &LangFeatures::for_release(
                KotlinVersion::V2_5_0_RC,
                LanguageVersion::V2_6,
                LanguageVersion::V2_6,
            ),
        );
        assert!(at_since.language.forbid_useless_type_arguments);
    }

    #[test]
    fn sam_constructor_missing_dependency_follows_the_release_that_introduced_it() {
        let mut before = File::default();
        apply_file_features(
            &mut before,
            &LangFeatures::for_release(
                KotlinVersion::V2_4_21,
                LanguageVersion::V2_4,
                LanguageVersion::V2_4,
            ),
        );
        assert_eq!(before.language.sam_constructor_missing_dependency, None);

        let mut rc = File::default();
        apply_file_features(
            &mut rc,
            &LangFeatures::for_release(
                KotlinVersion::V2_5_0_RC,
                LanguageVersion::V2_5,
                LanguageVersion::V2_5,
            ),
        );
        assert_eq!(rc.language.sam_constructor_missing_dependency, Some(false));

        let mut at_since = File::default();
        apply_file_features(
            &mut at_since,
            &LangFeatures::for_release(
                KotlinVersion::V2_5_0_RC,
                LanguageVersion::V2_6,
                LanguageVersion::V2_6,
            ),
        );
        assert_eq!(
            at_since.language.sam_constructor_missing_dependency,
            Some(true)
        );
    }
}
