# Parser and AST conformance

## Gate

The syntax-only corpus gate is:

```sh
./run-tests.sh --survey --parse-only --report /tmp/krusty-parse.tsv
```

It provisions the pinned Kotlin 2.4.10 `codegen/box` corpus, applies `prepare_test_source`, uses the
shared `// MODULE:` / `// FILE:` splitter, ignores non-Kotlin blocks, and parses every Kotlin block
with the case's language features. A successful parse must end at the final EOF and pass arena,
reference, list, operator/name-span, and source-range integrity validation. The TSV identifies the
exact corpus file and logical block as well as the stage, line, column, diagnostic, and source line.

Backend applicability is intentionally not consulted. The current corpus has 7,352 cases and 9,067
Kotlin source blocks.

## Failure inventory and ownership

The applicable-corpus baseline had 38 parse-first failures. Enabling the backend-independent gate
also exposed syntax in backend-inapplicable cases. The failures belong to these grammar productions;
the “owned regression” column names repository tests in `parser::tests` (with existing end-to-end
tests noted where useful).

| Grammar owner | Corpus forms/witnesses | Parser/AST representation | Owned regression |
| --- | --- | --- | --- |
| Type grammar and class headers | Function-type supertypes, suspend/big-arity function supertypes, repeated function-type `where` bounds, annotated supertypes; `coroutines/deserializedSuspendFunctionProperty.kt`, `reflection/typeOf/nonReifiedTypeParameters/upperBoundUsesOuterClassParameter.kt` | Complete `TypeRef` shape and all bounds; ordinary arities retain the `FunctionN` classifier while preserving function flags/parameters | `parser_retains_function_supertypes_multiple_bounds_and_property_forms`; `function_type_supertype_e2e` |
| Declaration and member grammar | `fun interface`; companion blocks; nested declarations; extension receivers and receiver annotations; enum `constructor`; multiline typealias targets; nested/local typealiases | Real classifier/member declarations, `ClassDecl::type_aliases`, `File::type_alias_decls`, and `Stmt::LocalTypeAlias`; no member-range skipping | `parser_retains_function_supertypes_multiple_bounds_and_property_forms`; `parser_retains_extension_access_compound_target_and_label_spans`; `malformed_nested_typealias_reports_the_complete_exact_diagnostic`; `nested_decls_in_object_e2e` |
| Property grammar | Missing initializers; abstract/expect/external/lateinit properties; delegated properties with newline before `by`; default accessors; extension-property `where` clauses; `companionBlocksAndExtensions/lateinit.kt`, `delegatedProperty/kt9712.kt` | `PropDecl::init == None`, explicit delegate/accessor facts, preserved modifiers and bounds; declaration-context validity is deferred to semantic phases | `parser_retains_function_supertypes_multiple_bounds_and_property_forms` |
| Expression and statement grammar | Extension-function expression invocation; anonymous-function/literal forms; safe-call compound assignment/inc-dec; call-valued compound targets; labelled declarations; local aliases; `extensionFunctions/*`, `functions/functionExpression/*`, `safeCall/parenthesizedSafeCallsAndOperators.kt`, `objects/compoundAssignmentToObjectFromCall.kt`, `labels/labeledDeclarations.kt`, `typealias/localTypeAliases.kt` | `Expr::ExtensionAccess`, `Stmt::CompoundAssign`, safe member assignment, `Stmt::LocalTypeAlias`, and exact sparse label/operator spans | `parser_retains_extension_access_compound_target_and_label_spans` |
| Delimiters and newline-sensitive recovery | Parenthesized `if`/`try` bound followed on a new line by `downTo`; declaration boundary ambiguity; missing alias `=` and missing delimiters; `ranges/kt37370.kt` | Plain newlines are accepted only at grammar-defined positions; semicolons remain boundaries; invalid neighbors retain exact diagnostics | `multiline_control_expression_before_down_to_is_one_for_range`; `malformed_nested_typealias_reports_the_complete_exact_diagnostic`; existing nesting/recovery tests |

Measured all-backend deltas during implementation were 50 → 19 → 13 → 2 → 1 failing cases. The
last parser-owned valid case, `ranges/kt37370.kt`, was removed by the newline-sensitive range fix.

## Corpus defect preventing a truthful 7,352/7,352 result

The sole remaining case is
`contextParameters/withExtensionReceiverInType.kt`, block `withExtensionReceiverInType.kt`. Lines 44
and 46 contain extra closing parentheses. The pinned Kotlin 2.4.10 compiler reports syntax errors at
exactly 44:59 and 46:57 (“Unexpected tokens”), matching Krusty's delimiter rejection. The file is
marked backend-inapplicable, but backend applicability cannot make invalid Kotlin syntax valid.

Consequently the honest gate result is currently:

```text
Discovered cases: 7352
Kotlin blocks:    9067
Parsed cases:     7351
Lex failures:        0
Parse failures:      1
AST failures:        0
Panics:              0
```

Accepting that file would require one of the forbidden approaches: a corpus-path exception, silent
delimiter recovery reported as success, or weakened invalid-syntax diagnostics. The gate therefore
keeps the defect visible. Once the pinned input is corrected, no parser change should be required.

## Semantic boundary

Valid syntax no longer fails in the parser merely because a later phase lacks support. In particular,
initializer-less/default/delegated properties, function-type supertypes, fun interfaces, companion
blocks, local/nested aliases, extension-expression invocation, and general compound-assignment
targets all reach a complete AST. New syntax variants have explicit checker/lowering adaptation;
capability diagnostics occur after parsing.
