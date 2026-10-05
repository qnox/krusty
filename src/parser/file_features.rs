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
    file.enum_entries_enabled = features.has("EnumEntries");
    file.prioritized_enum_entries = features.has("PrioritizedEnumEntries");
    file.implicit_signed_to_unsigned_integer_conversion =
        features.has("ImplicitSignedToUnsignedIntegerConversion");
    file.data_copy_respects_ctor_visibility =
        features.has("DataClassCopyRespectsConstructorVisibility");
    file.unit_conversions_on_arbitrary_expressions =
        features.has("UnitConversionsOnArbitraryExpressions");
    file.eager_lambda_analysis = features.has("EagerLambdaAnalysis");
    file.opted_in_markers = features.opted_in().map(str::to_string).collect();
}
