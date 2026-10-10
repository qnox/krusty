//! Language-policy bits a parsed file carries into later phases.

use crate::ast::File;
use crate::ast::{ValueClassArity, ValueClassRules};
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
    file.destructuring.warns_parenthesized_short_form = features
        .has("DeprecateNameMismatchInShortDestructuringWithParentheses")
        && !features.has("EnableNameBasedDestructuringShortForm");
    file.lambda_implementation_uses_inferred_result =
        features.language_version() < crate::language_version::LanguageVersion::V2_4;
    file.full_value_classes = features.has("FullValueClasses");
    file.recursive_type_of = features.has("JvmSupportRecursiveTypeOf");
    file.opted_in_markers = features.opted_in().map(str::to_string).collect();
    let gates = &mut file.language_gates;
    gates.companion_blocks_and_extensions = features.gate("CompanionBlocksAndExtensions");
    gates.collection_literals = features.gate("CollectionLiterals");
    gates.functional_type_with_extension_as_supertype =
        features.gate("FunctionalTypeWithExtensionAsSupertype");
    gates.enum_entry_named_entries = features.deprecation("ForbidEnumEntryNamedEntries");
    gates.intersection_reified_type_parameter =
        features.deprecation("ProhibitIntersectionReifiedTypeParameter");
    gates.intrinsic_const_evaluation = features.has("IntrinsicConstEvaluation");
    gates.value_classes = ValueClassRules {
        arity: if features
            .table()
            .get("JvmInlineMultiFieldValueClasses")
            .is_some()
        {
            ValueClassArity::MultiFieldFeature {
                enabled: features.has("JvmInlineMultiFieldValueClasses"),
            }
        } else {
            ValueClassArity::Single {
                full_value_classes: features.gate("FullValueClasses"),
            }
        },
        custom_equals: features.has("CustomEqualsInValueClasses"),
        expect_without_primary_constructor: features
            .has("AllowExpectValueClassesWithNoPrimaryConstructor"),
    };
}
