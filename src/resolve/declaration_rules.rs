//! Declaration-time validity rules that a declaration's own shape and host decide: `tailrec` on
//! open members and the hosts `@JvmStatic` admits.

mod jvm_static;
mod tailrec;

pub(super) use jvm_static::JvmStaticHost;

use super::{Checker, CheckerScope};
use crate::ast::{ClassDecl, DeclId, FunDecl};
use crate::types::TypeName;

impl Checker<'_> {
    /// The member rules a class body carries.
    pub(super) fn check_member_declaration_rules(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
        owner: Option<TypeName>,
        anonymous: bool,
    ) {
        self.report_local_enum(class);
        tailrec::check_members(self.diags, class);
        let host = self.jvm_static_host(class, owner, anonymous);
        for method in &class.methods {
            self.check_jvm_static_function(scope, method, host);
        }
        // An enum entry's body is an anonymous subclass, which no `@JvmStatic` member fits.
        for method in class.enum_entries.iter().flat_map(|entry| &entry.methods) {
            self.check_jvm_static_function(scope, method, JvmStaticHost::Elsewhere);
        }
    }

    /// `enum` does not apply to a local class. The report sits on the `enum` keyword.
    fn report_local_enum(&mut self, class: &ClassDecl) {
        if !class.is_enum() {
            return;
        }
        let Some(declaration) = self.file.decls.iter().copied().find(|&id| {
            matches!(
                self.file.decl(id),
                crate::ast::Decl::Class(found) if found.span == class.span
            )
        }) else {
            return;
        };
        if !self.file.is_local_declaration(declaration) {
            return;
        }
        // A local classifier's prefix is not recorded: the parser consumes it before the enum
        // parser, and that parser's span starts on the `enum` keyword.
        let keyword = self
            .file
            .declaration_prefixes
            .at(class.name_span.lo)
            .and_then(|prefix| {
                self.file
                    .declaration_prefixes
                    .keywords(prefix)
                    .iter()
                    .find(|(modifier, _)| *modifier == crate::ast::ModifierKeyword::Enum)
                    .map(|(_, span)| *span)
            })
            .unwrap_or_else(|| {
                // `enum` is four source bytes, and the class span starts on that keyword.
                crate::diag::Span::new(class.span.lo, class.span.lo.saturating_add(4))
            });
        self.diags.error(
            keyword,
            "modifier 'enum' is not applicable to 'local class'.",
        );
    }

    /// Check a top-level function, whose `@JvmStatic` has no host.
    pub(super) fn check_top_level_function(
        &mut self,
        scope: &CheckerScope<'_>,
        function: &FunDecl,
        declaration: DeclId,
    ) {
        self.check_jvm_static_function(scope, function, JvmStaticHost::Elsewhere);
        self.check_top_level_fun(scope, function, declaration);
    }
}
