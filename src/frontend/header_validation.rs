//! Semantic validation owned by declaration headers.
//!
//! These checks run while one bounded Pass-1 parser unit is live. They must not depend on a body
//! visit: declarations such as a class with only primary-constructor parameters have no ordinary
//! Pass-2 body unit at all.

use crate::ast::{Decl, File};
use crate::diag::DiagSink;
// kotlinc's three sentences for an `expect` declaration that carries an implementation, worded per
// reference version. None varies with the declaration's kind.
use crate::diagnostic_wording::{
    expected_declaration_with_body as body_message,
    expected_delegated_property as delegate_message,
    expected_property_initializer as initializer_message,
};

pub(super) fn validate(file: &File, diagnostics: &mut DiagSink) {
    for alias in &file.type_alias_decls {
        validate_type_alias(alias, diagnostics);
    }
    for declaration in &file.decl_arena {
        match declaration {
            Decl::Fun(function) => validate_function(function, false, diagnostics),
            Decl::Property(_) => {}
            Decl::Class(class) => {
                let mut constructor_parameters = crate::fx_hash::HashSet::default();
                for parameter in &class.props {
                    if !constructor_parameters.insert(parameter.name.as_str()) {
                        diagnostics.error(
                            class.span,
                            format!(
                                "conflicting declaration: constructor parameter '{}' is declared more than once",
                                parameter.name
                            ),
                        );
                    }
                }
                for diagnostic in crate::declaration_validation::enum_constructor_diagnostics(class)
                {
                    diagnostics.error(diagnostic.span, diagnostic.message.to_string());
                }
                for function in &class.methods {
                    validate_function(function, true, diagnostics);
                }
                for alias in &class.type_aliases {
                    validate_type_alias(alias, diagnostics);
                }
                if class.is_interface() {
                    for property in &class.body_props {
                        for diagnostic in
                            crate::declaration_validation::interface_property_diagnostics(property)
                        {
                            diagnostics.error(diagnostic.span, diagnostic.message.to_string());
                        }
                    }
                    validate_interface_block_members(file, class, diagnostics);
                }
                for entry in &class.enum_entries {
                    for function in &entry.methods {
                        validate_function(function, true, diagnostics);
                    }
                }
            }
        }
    }
}

/// These semantic errors do not invalidate the parsed declaration inventory or suppress body
/// checking. Report them only after the caller records its parser-failure state.
pub(super) fn validate_after_parsing(file: &File, parser_error: bool, diags: &mut DiagSink) {
    // The brackets were parsed. Report the language gate only after `parse_error` is captured,
    // so the file's declarations still join the module for every other file.
    for span in file
        .destructuring
        .ungated_bracket_spans
        .iter()
        .copied()
        .chain(file.destructuring.ungated_full_form_spans.iter().copied())
    {
        diags.error(
            span,
            "the feature \"name based destructuring\" is only available since language version 2.5",
        );
    }
    for span in file.destructuring.ungated_short_form_spans.iter().copied() {
        diags.error(
            span,
            "the feature \"enable name based destructuring short form\" is experimental and \
             should be enabled explicitly. This can be done by supplying the compiler \
             argument '-Xname-based-destructuring=complete', but note that no stability \
             guarantees are provided.",
        );
    }
    // The body check does not run for a syntax-error file. Members still owe the same body
    // requirement as a top-level function; only the sentence differs. The top-level sentence is
    // reported for a syntax-error file too — `fun broken(` at EOF is still a function without a
    // body. A script keeps its previous exemption.
    if parser_error {
        report_member_functions_without_bodies(file, diags);
    }
    if !file.is_script {
        report_top_level_functions_without_bodies(file, diags);
    }
}

fn function_body_status(
    function: &crate::ast::FunDecl,
    is_abstract: bool,
    is_expect: bool,
) -> crate::declaration_validation::FunctionBodyStatus {
    crate::declaration_validation::FunctionBodyStatus {
        has_body: !matches!(function.body, crate::ast::FunBody::None),
        is_abstract,
        is_external: function.is_external(),
        is_expect,
        is_companion_block_member: function.is_companion_block_member(),
    }
}

fn declaration_is_expect(file: &File, declaration: crate::ast::DeclId) -> bool {
    file.expect_decls
        .iter()
        .any(|expected| expected.declaration == declaration)
}

fn report_top_level_functions_without_bodies(file: &File, diags: &mut DiagSink) {
    for &declaration in &file.decls {
        let Decl::Fun(function) = file.decl(declaration) else {
            continue;
        };
        // A failed name parse (`fun` with nothing after it) is not a function this requirement
        // reports. That file's diagnostics stay the three syntax errors the recovery test pins.
        if function.name == "<error>" {
            continue;
        }
        if !crate::declaration_validation::function_requires_body(function_body_status(
            function,
            function.is_abstract(),
            declaration_is_expect(file, declaration),
        )) {
            continue;
        }
        diags.error_named(
            function.span,
            crate::diag::names::NON_MEMBER_FUNCTION_NO_BODY,
            format!("function '{}' must have a body.", function.name),
        );
    }
}

fn report_member_functions_without_bodies(file: &File, diagnostics: &mut DiagSink) {
    for &declaration in &file.decls {
        let Decl::Class(class) = file.decl(declaration) else {
            continue;
        };
        let owner_is_expect = declaration_is_expect(file, declaration);
        for function in &class.methods {
            if function.name == "<error>" {
                continue;
            }
            // Interface members are abstract by the classifier's kind. An expect classifier's
            // members are expect declarations even though each one is not marked abstract.
            if !crate::declaration_validation::function_requires_body(function_body_status(
                function,
                function.is_abstract() || class.is_interface(),
                owner_is_expect,
            )) {
                continue;
            }
            diagnostics.error_named(
                function.span,
                crate::diag::names::NON_ABSTRACT_FUNCTION_WITH_NO_BODY,
                format!(
                    "function '{}' without a body must be abstract.",
                    function.name
                ),
            );
        }
    }
}

fn validate_type_alias(alias: &crate::ast::TypeAliasDecl, diagnostics: &mut DiagSink) {
    for diagnostic in crate::declaration_validation::type_alias_diagnostics(alias) {
        diagnostics.error(diagnostic.span, diagnostic.message.to_string());
    }
}

fn validate_interface_block_members(
    file: &File,
    class: &crate::ast::ClassDecl,
    diagnostics: &mut DiagSink,
) {
    if file.language.interface_companion_block_members_are_static {
        return;
    }
    for member in &class.companion_block_members {
        let Decl::Property(property) = file.decl(member.declaration) else {
            continue;
        };
        if let Some(diagnostic) =
            crate::declaration_validation::interface_block_property_diagnostics(
                property,
                member.private_modifier,
            )
        {
            diagnostics.error(diagnostic.span, diagnostic.message.to_string());
        }
    }
}

fn validate_function(function: &crate::ast::FunDecl, member: bool, diagnostics: &mut DiagSink) {
    for diagnostic in crate::declaration_validation::infix_declaration_diagnostics(function, member)
    {
        diagnostics.error(diagnostic.span, diagnostic.message.to_string());
    }
}

/// Report every `expect` declaration that carries an implementation, and say whether it found one.
///
/// A header declares; it does not implement. The reference compiler reports this whether or not
/// the multiplatform feature is on — the two diagnostics are independent, and a file without the
/// feature gets both — so this check is purely syntactic and runs unconditionally.
///
/// Positions are measured, not guessed: a top-level function is reported at its `expect` keyword, a
/// member at its own declaration, an accessor at the accessor header (`get()` / `set(v)`), and a
/// property INITIALIZER at the initializer expression rather than at the property. A secondary
/// constructor with a body inside an `expect class` is deliberately absent: the reference compiler
/// does not report it, though an `init` block in the same position it does.
pub(super) fn validate_expect_bodies(file: &File, diagnostics: &mut DiagSink) -> bool {
    let before = diagnostics.diags.len();
    for expect in &file.expect_decls {
        match file.decl(expect.declaration) {
            Decl::Fun(function) => {
                // The whole declaration is the header, so the keyword that introduced it is where
                // the reference compiler points. It travels with the declaration; nothing here
                // searches for it, and nothing substitutes another position when it is absent.
                if !matches!(function.body, crate::ast::FunBody::None) {
                    diagnostics.error(expect.keyword, body_message(file.language.reference));
                }
            }
            Decl::Property(property) => {
                validate_expect_property(file, property, expect.keyword, diagnostics);
            }
            Decl::Class(class) => validate_expect_class(file, class, diagnostics),
        }
    }
    diagnostics.diags.len() > before
}

/// Members of an `expect` classifier are headers too, each reported at its own declaration.
fn validate_expect_class(file: &File, class: &crate::ast::ClassDecl, diagnostics: &mut DiagSink) {
    for function in &class.methods {
        if !matches!(function.body, crate::ast::FunBody::None) {
            diagnostics.error(
                function.signature_span,
                body_message(file.language.reference),
            );
        }
    }
    for property in &class.body_props {
        validate_expect_property(file, property, property.span, diagnostics);
    }
    for step in &class.init_order {
        if let crate::ast::ClassInit::Block(body) = step {
            // At the `init` keyword, not at the `{` the block expression's own span starts on.
            // The parser records that keyword for every `init` block it parses, so an absent one is
            // a broken parse product rather than a block to skip: reporting nothing here would drop
            // the diagnostic for an implementation the header really carries.
            match file.init_blocks.get(body) {
                Some(header) => {
                    diagnostics.error(header.keyword, body_message(file.language.reference))
                }
                None => diagnostics.error(
                    class.span,
                    "internal error: an init block in an expect classifier has no `init` keyword",
                ),
            }
        }
    }
    for entry in &class.enum_entries {
        for function in &entry.methods {
            if !matches!(function.body, crate::ast::FunBody::None) {
                diagnostics.error(
                    function.signature_span,
                    body_message(file.language.reference),
                );
            }
        }
    }
}

/// A property header may carry four separable implementations — an initializer, a delegate, and
/// either accessor — and the reference compiler reports each one where it is written.
fn validate_expect_property(
    file: &File,
    property: &crate::ast::PropDecl,
    declaration: crate::diag::Span,
    diagnostics: &mut DiagSink,
) {
    // Each of these positions is a parse product of the very syntax being reported, so an absent
    // one is a broken arena rather than a reason to relocate the diagnostic. Reporting it at the
    // declaration instead would put the message somewhere the reference compiler never puts it,
    // and the complete ordered ledger the regressions compare would silently stop matching.
    let missing = |what: &str| format!("internal error: an expect property's {what} has no span");
    if let Some(init) = property.init {
        // At the initializer EXPRESSION: `expect val x: Int = 3` is reported under the `3`.
        match file.expr_span(init) {
            Some(at) => diagnostics.error(at, initializer_message(file.language.reference)),
            None => diagnostics.error(declaration, missing("initializer")),
        }
    }
    if let Some(delegate) = property.delegate {
        // A delegate is an implementation too, and gets its own sentence — at the delegate
        // EXPRESSION (`by lazy { 1 }` is reported under `lazy { 1 }`).
        match file.expr_span(delegate) {
            Some(at) => diagnostics.error(at, delegate_message(file.language.reference)),
            None => diagnostics.error(declaration, missing("delegate")),
        }
    }
    if property.getter.is_some() {
        match property.getter_span {
            Some(at) => diagnostics.error(at, body_message(file.language.reference)),
            None => diagnostics.error(declaration, missing("getter")),
        }
    }
    if let Some(setter) = &property.setter {
        if setter.body.is_some() {
            diagnostics.error(setter.span, body_message(file.language.reference));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::diag::{DiagSink, Severity};
    use crate::features::LangFeatures;
    use crate::source::SourceInput;

    #[test]
    fn a_declaration_error_retains_the_inventory_and_checked_body_in_trimmed_analysis() {
        for source in [
            "fun missing(): String\nval wrong: String = 1\n",
            "val wrong: String = 1\nfun missing(): String\n",
        ] {
            let inputs = [SourceInput::kotlin(source)];
            let mut diagnostics = DiagSink::new();
            let analysis = crate::frontend::analyze_source_set_prefix_with_features_trimmed(
                &inputs,
                1,
                1,
                crate::frontend::PlatformProvider::jvm(Box::new(
                    crate::libraries::EmptySymbolSource,
                )),
                &LangFeatures::new(),
                &mut diagnostics,
            );
            assert!(analysis.streamed.is_some());
            assert!(analysis.types[0].is_some());
            assert_eq!(diagnostics.diags.len(), 2, "{:?}", diagnostics.diags);
            assert!(diagnostics.diags.iter().all(|diagnostic| {
                diagnostic.file == 0 && diagnostic.severity == Severity::Error
            }));
            let messages = diagnostics
                .diags
                .iter()
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                messages,
                [
                    "function 'missing' must have a body.",
                    "initializer type mismatch: expected 'String', actual 'Int'.",
                ]
            );
        }
    }
}
