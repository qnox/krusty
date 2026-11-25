//! The language-feature policy a parsed file carries into later phases.
//!
//! Each field is projected once from the compilation's language features (see
//! `parser::file_features`), so parsing, checking and lowering read the same decision.

/// One file's language-feature decisions.
#[derive(Default)]
pub struct FileLanguagePolicy {
    /// `+ExplicitContextArguments`: named context parameters may be supplied at a call site instead
    /// of being selected from the implicit receiver and lexical scope.
    pub explicit_context_arguments: bool,
    /// `+CallableReferencesToContextual`: a reference to a declaration with context parameters
    /// selects those arguments from the reference site and exposes only the value parameters.
    pub callable_references_to_contextual: bool,
    /// `+ContextSensitiveResolutionUsingExpectedType`: an otherwise-unbound simple classifier or
    /// classifier value may be selected from the expected type's nested classifier hierarchy.
    pub context_sensitive_resolution_using_expected_type: bool,
    /// `+EnforceMissingNamedArgumentsOnJavaAnnotation`: a positional argument of a Java annotation
    /// written as a constructor call (a nested annotation argument) is an error, as it already is
    /// on a top-level annotation use.
    pub enforce_missing_named_arguments_on_java_annotation: bool,
    /// The warning text kotlinc reports for that argument while the feature is off; `None` when the
    /// release predates it and accepts the argument silently.
    pub positional_java_annotation_call_argument_warning: Option<String>,
    /// `+ReportSubclassCantCallCompanionProtectedNonStaticWithGenerics`: a subclass's use of a
    /// protected, non-static companion member of a generic superclass is reported like the
    /// non-generic case.
    pub report_subclass_cant_call_companion_protected_with_generics: bool,
    /// `+BareArrayClassLiteral`: permit builtin `Array` without explicit type arguments on the
    /// left of `::class`; semantic resolution supplies its star projection.
    pub bare_array_class_literal: bool,
    /// `+ForbidArrayOfNothingInLhsOfClassLiteral`: a class literal of `Array<Nothing>` or
    /// `Array<Nothing?>` is an error rather than a warning.
    pub forbid_array_of_nothing_in_lhs_of_class_literal: bool,
    /// `+EnumEntries`: expose the synthetic classifier property `E.entries` on enum classes.
    pub enum_entries_enabled: bool,
    /// `+CollectionLiterals`: uses of `kotlin.ExperimentalCollectionLiteralsApi` declarations are
    /// accepted without an opt-in, as kotlinc's opt-in usage check does.
    pub collection_literals_enabled: bool,
    /// `+PrioritizedEnumEntries`: the synthetic enum property wins over a same-named companion
    /// declaration. With the legacy feature disabled, the companion rung is considered first.
    pub prioritized_enum_entries: bool,
    /// Whether signed integer constant expressions may adapt to unsigned parameters explicitly
    /// marked with `kotlin.internal.ImplicitIntegerCoercion`.
    pub implicit_signed_to_unsigned_integer_conversion: bool,
    /// `+DataClassCopyRespectsConstructorVisibility` (`-Xconsistent-data-class-copy-visibility`):
    /// a data class's synthesized `copy`/`copy$default` take the primary constructor's visibility
    /// instead of being unconditionally public. Signature collection combines it with the per-class
    /// `kotlin.ConsistentCopyVisibility` / `kotlin.ExposedCopyVisibility` annotations.
    pub data_copy_respects_ctor_visibility: bool,
    /// Configured property-parameter annotation defaulting, projected once from language features.
    pub property_annotation_default_target_mode: crate::types::PropertyAnnotationDefaultTargetMode,
    /// `+UnitConversionsOnArbitraryExpressions`: a call argument whose function value returns
    /// non-`Unit` may be converted to a function type returning `Unit`.
    pub unit_conversions_on_arbitrary_expressions: bool,
    /// `+EagerLambdaAnalysis`: a lambda that does not discriminate applicable candidates by its
    /// shape is analyzed before the most specific candidate is chosen.
    pub eager_lambda_analysis: bool,
    /// `+AllowEagerSupertypeAccessibilityChecks`: a missing supertype found at a constructor call
    /// (or behind a dispatch receiver already reported) is an error rather than a warning.
    pub allow_eager_supertype_accessibility_checks: bool,
    /// `ForbidSamConstructorCallsWithMissingDependencySupertype` when the reference release has
    /// it. `None` on a release without the check. `Some(false)` warns at a SAM constructor or
    /// SAM conversion whose interface is missing a dependency supertype; `Some(true)` makes that
    /// an error.
    pub sam_constructor_missing_dependency: Option<bool>,
    /// Before Kotlin language level 2.4, a lambda implementation keeps the body's inferred result
    /// separately from the caller-facing function result. The checked FIR boundary records that
    /// semantic choice so a backend never needs its own language-version switch.
    pub lambda_implementation_uses_inferred_result: bool,
    /// `+FullValueClasses`: a `value class` without `@JvmInline` is a boxed class with structural
    /// `equals`/`hashCode`/`toString`. Without the feature that declaration is an error.
    pub full_value_classes: bool,
    /// `+ExplicitInlineValueClassesSyntax`: `inline value class` declares an inline class.
    pub explicit_inline_value_classes: bool,
    /// `+AllowSmartCastsOnValueClassUnderlyingProperties`: a value class's component declared in
    /// another module is a stable smart-cast subject.
    pub smart_cast_value_class_components: bool,
    /// `+ForbidValueClassRecursionViaTypeParameters`: a value class whose underlying type reaches
    /// itself through a type parameter's bound is an error rather than a deprecation warning.
    pub forbid_value_class_recursion_via_type_parameters: bool,
    /// The exact deprecation message selected by this compilation's reference release. Releases
    /// before the warning/error split leave this absent and report the construct as a plain error.
    pub value_class_recursion_via_type_parameters_warning: Option<String>,
    /// Declaration-use deprecation migrations selected at the language-feature boundary.
    pub deprecation_migrations: crate::features::DeprecationMigrations,
    /// `+ForbidOperatorEqualsInEnumEntriesAndAnonymousObjects`: a non-overriding `operator fun
    /// equals` in an enum entry or anonymous object is an error rather than a deprecation warning.
    pub forbid_operator_equals_in_enum_entries_and_anonymous_objects: bool,
    /// Exact per-compilation warning text for that transition. Releases before the diagnostic split
    /// leave this absent and report the construct as an ordinary error.
    pub inapplicable_operator_modifier_warning: Option<String>,
    /// `+NormalizeNaNValuesInConstContext`: every NaN a compile-time constant evaluates to is the
    /// canonical NaN of its type, whatever sign and payload the arithmetic produced.
    pub normalize_nan_in_const_context: bool,
    /// `+NoWhenBranchMatchedExceptionWithMessage`: an exhaustive subject `when` without an `else`
    /// fails through `kotlin.internal.throwNoWhenBranchMatchedException(subject)`.
    pub no_when_branch_matched_with_subject: bool,
    /// `+IntrinsicConstEvaluation`: a constant context (an annotation argument, a `const val`)
    /// also evaluates the intrinsic-const reads it otherwise rejects, such as an entry's `name`.
    pub intrinsic_const_evaluation: bool,
    /// `+StrictEquals` on a release with strict equality for structural classes: the generated
    /// `equals` of a data or value class is bound to its own class, so an equality that holds
    /// proves the other operand has that class.
    pub structural_equality_bound: bool,
    /// The release declares `CompanionBlocks` apart from `CompanionExtensions`: a `companion { … }`
    /// block member of an interface is a static of that interface, so an initialized `private`
    /// block property is an ordinary private static. A release that only has the combined
    /// `CompanionBlocksAndExtensions` treats it as an abstract interface member instead.
    pub interface_companion_block_members_are_static: bool,
    /// `+CustomEqualsInValueClasses`: permit the typed `equals(Self)` convention in a value class
    /// whose selected representation is inline rather than full/boxed.
    pub custom_equals_in_value_classes: bool,
    /// `+ForbidUpperBoundsViolationOnTypeOperatorAndParameterBounds`: the outermost application of
    /// a type operator's target and of a callable's type-parameter bound is bound-checked too.
    pub forbid_upper_bounds_violation_on_type_operator_and_parameter_bounds: bool,
    /// `+ForbidUselessTypeArgumentsIn25`: type arguments that select nothing (on a qualifier reaching
    /// a static member, …) are errors rather than deprecation warnings.
    pub forbid_useless_type_arguments: bool,
    /// Release-owned warning formatting for class-literal diagnostics whose declaration-specific
    /// leading message is known only after classifier selection.
    pub useless_type_arguments_warning_format: Option<crate::features::DeprecationWarningFormat>,
    /// `+ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier`: an annotated or
    /// type-argument segment cannot name a package, and a parenthesized package qualifier is an
    /// error rather than a deprecation warning.
    pub forbid_package_qualifier_forms: bool,
    /// Exact warning text for a parenthesized package qualifier before that transition; `None` for
    /// a release without it.
    pub parenthesized_package_qualifier_warning: Option<String>,
    /// Exact warning text for type arguments on an earlier package segment before that
    /// transition; `None` for a release without it.
    pub package_qualifier_type_arguments_warning: Option<String>,
    /// Exact per-compilation warning text for the useless-type-argument transition.
    pub type_arguments_not_allowed_warning: Option<String>,
    /// Exact warning text when the same transition applies to a standalone singleton value.
    pub explicit_type_arguments_in_property_access_warning: Option<String>,
    /// `+ForbidJavaClassPropertyReferences`: a reference to the stdlib `javaClass` extension
    /// property is an error rather than a deprecation warning.
    pub forbid_java_class_property_references: bool,
    /// Exact warning text for that reference before the transition; `None` for a release that
    /// predates the diagnostic, which reports nothing.
    pub java_class_property_reference_warning: Option<String>,
    /// `+ErrorAboutDataClassCopyVisibilityChange`: a `data class` whose generated `copy()` exposes its
    /// non-public primary constructor, and a use of that `copy()`, are errors rather than warnings.
    pub error_about_data_class_copy_visibility_change: bool,
    /// Exact per-compilation deprecation text for the non-public-constructor declaration warning.
    pub data_class_copy_visibility_change_warning: Option<String>,
    /// Exact per-compilation deprecation text for an inaccessible generated `copy()` usage warning.
    pub data_class_invisible_copy_usage_warning: Option<String>,
    /// `+ReportTypeVarianceConflictsInDnnAndFlexible`: the class variance checker also walks a
    /// definitely-non-null type and a flexible (platform) type. Without it those types are skipped.
    pub report_type_variance_conflicts_in_dnn_and_flexible: bool,
    /// The language features deciding which reified type arguments are reported, and how.
    pub reified_type_arguments: crate::features::ReifiedTypeArgumentFeatures,
    /// `+JvmSupportRecursiveTypeOf`: permit reflective descriptions of non-reified type
    /// parameters whose declared upper bounds contain a cycle.
    pub recursive_type_of: bool,
    /// Opt-in markers accepted module-wide (`-opt-in`), as dotted fully qualified names.
    pub opted_in_markers: Vec<String>,
}
