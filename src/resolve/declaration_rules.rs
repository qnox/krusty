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
