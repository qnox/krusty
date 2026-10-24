//! Inline dependency functions whose calls this file expands.
//!
//! A checked call of an inline dependency function is expanded at the call site as a call of a
//! same-module inline function is, from a template function in this file's common IR. Common
//! lowering does not read dependencies: the compiler supplies the templates through
//! [`DependencyInlineTemplates`], which lowers a dependency's own inline body into the file, and
//! lowering asks for each checked callee once per file.

use std::rc::Rc;

use crate::fir::ExternalCallableId;
use crate::fx_hash::HashMap;
use crate::ir::{FunId, IrFile};

/// A template a call of an inline dependency function expands.
#[derive(Clone, Debug)]
pub struct DependencyInlineTemplate {
    /// The inline-only function in this file whose body is the dependency's.
    pub function: FunId,
    /// The dependency function's source name, which its expansion frames are opened under.
    pub name: String,
    /// The semantic identities of the dependency function's type parameters, in declaration
    /// order: the template's types name them, and a call's substitutions bind them by ordinal.
    pub type_parameters: Vec<String>,
}

/// The compiler's source of templates for inline dependency functions.
pub trait DependencyInlineTemplates {
    /// Lower the inline body of the dependency function `target` into `ir`, or `None` when it has
    /// no body this compilation can lower as a template. A declined template leaves `ir` as it
    /// was.
    fn lower(
        &self,
        ir: &mut IrFile,
        target: ExternalCallableId,
    ) -> Option<DependencyInlineTemplate>;

    /// Whether the compiler, not the body, gives `target` its meaning: a compiler intrinsic such
    /// as `typeOf`, whose serialized body only throws. Its call stays the checked call each target
    /// realizes, and is never expanded from a template. An intrinsic with a real body, which a
    /// target may realize only in part, keeps its template.
    fn compiler_implemented(&self, target: ExternalCallableId) -> bool;
}

/// The templates one file's lowering has asked for, each lowered at most once.
#[derive(Clone, Default)]
pub(crate) struct DependencyTemplateCache {
    source: Option<Rc<dyn DependencyInlineTemplates>>,
    lowered: HashMap<ExternalCallableId, Option<DependencyInlineTemplate>>,
}

impl std::fmt::Debug for DependencyTemplateCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DependencyTemplateCache")
            .field("source", &self.source.is_some())
            .field("lowered", &self.lowered)
            .finish()
    }
}

impl DependencyTemplateCache {
    pub(crate) fn provide(&mut self, source: Rc<dyn DependencyInlineTemplates>) {
        self.source = Some(source);
    }

    /// Whether this compilation has a template source. A compilation without one, such as one
    /// whose dependencies are classfiles, calls every dependency function as declared.
    pub(crate) fn provides_templates(&self) -> bool {
        self.source.is_some()
    }

    /// Whether `target`'s call is the compiler's to realize rather than its body's to expand.
    pub(crate) fn compiler_implemented(&self, target: ExternalCallableId) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| source.compiler_implemented(target))
    }

    /// The template of `target` in `ir`, lowering it the first time it is asked for.
    pub(crate) fn template(
        &mut self,
        ir: &mut IrFile,
        target: ExternalCallableId,
    ) -> Option<&DependencyInlineTemplate> {
        let source = self.source.as_ref()?;
        self.lowered
            .entry(target)
            .or_insert_with(|| source.lower(ir, target))
            .as_ref()
    }
}
