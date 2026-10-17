//! Whether a lambda is written inside an `inline` declaration.
//!
//! kotlinc compiles every lambda written in the body of an `inline` function to a class of its
//! own, never to an `invokedynamic` of `LambdaMetafactory`: `inline fun a() = take { "a" }` writes
//! `FileKt$a$1`. The rule follows the lexical scope outward through enclosing lambdas and through
//! the members of local and anonymous classes, so a lambda nested in another lambda or in an
//! object's method inside an inline function is a class too. A lambda written in an inline call's
//! lambda argument belongs to the caller's scope and is unaffected, and a lambda an inline call
//! splices in place is no value at all.
//!
//! A caller in the same file that inlines the function copies the lambda's value into its own
//! body. The copy's scope is the caller's, so only the declaration's own node reaches the inline
//! function; that node is the class's site.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{ClassId, ExprId, FunId, IrEnclosure, IrExpr, IrFile};

/// The inline function a lambda is written in.
#[derive(Clone, Copy)]
pub(super) struct InlineScope {
    /// The inline function.
    pub(super) owner: FunId,
    /// The lambda node the inline function's body declares.
    pub(super) node: ExprId,
    /// The lambda is written directly in the inline function's body, not nested in another lambda
    /// or class. kotlinc gives only such a class the source map of a class generated for inlining.
    pub(super) direct: bool,
    /// The lambda whose body declares this one, which is the class's enclosing method.
    pub(super) parent_lambda: Option<FunId>,
    /// The inline function is another source file's, retained here only to be inlined. That file
    /// declares the class; this one constructs it.
    pub(super) foreign: bool,
}

/// The lexical scopes of the file's lambdas and classes, answering [`Self::inline_scope`] for each
/// lambda without a walk of the whole file per query.
pub(super) struct InlineScopes {
    /// The scope of each node that builds a lambda implementation's value, in node order.
    lambda_scopes: HashMap<FunId, Vec<(ExprId, IrEnclosure)>>,
    /// The class declaring each member function.
    method_owners: HashMap<FunId, ClassId>,
}

impl InlineScopes {
    pub(super) fn new(ir: &IrFile) -> Self {
        let mut lambda_scopes = HashMap::<FunId, Vec<(ExprId, IrEnclosure)>>::default();
        for (node, expression) in ir.exprs.iter().enumerate() {
            let IrExpr::Lambda { impl_fn, .. } = expression else {
                continue;
            };
            let node = ExprId::try_from(node).expect("IR expression index exceeds ExprId");
            // An inlined copy has site-specific enclosure provenance; the source declaration has
            // the implementation's enclosure instead. These are the two valid representations of
            // the same bound lambda site, not alternative scope lookups after resolution.
            let scope = ir
                .callable_reference_enclosures
                .get(&node)
                .or_else(|| ir.lambda_enclosures.get(impl_fn));
            if let Some(&scope) = scope {
                lambda_scopes
                    .entry(*impl_fn)
                    .or_default()
                    .push((node, scope));
            }
        }
        let method_owners = ir
            .classes
            .iter()
            .enumerate()
            .flat_map(|(class, declaration)| {
                let class = ClassId::try_from(class).expect("IR class index exceeds ClassId");
                declaration
                    .methods
                    .iter()
                    .map(move |&method| (method, class))
            })
            .collect();
        Self {
            lambda_scopes,
            method_owners,
        }
    }

    /// The inline function `lambda` is written in, if any.
    pub(super) fn inline_scope(&self, ir: &IrFile, lambda: FunId) -> Option<InlineScope> {
        self.lambda_scopes
            .get(&lambda)?
            .iter()
            .find_map(|&(node, scope)| {
                let mut visited = HashSet::default();
                visited.insert(lambda);
                let owner = self.inline_owner(ir, scope, &mut visited)?;
                // An enclosure names the declaring function even for a lambda nested in another
                // one; only a node of the function's own body is written directly in it.
                let declares = |function: FunId| {
                    ir.functions[function as usize].body.is_some_and(|body| {
                        crate::ir::value_namespace_expressions(ir, body).contains(&node)
                    })
                };
                let direct = declares(owner);
                let parent_lambda = if direct {
                    None
                } else {
                    self.lambda_scopes
                        .keys()
                        .copied()
                        .filter(|&other| other != lambda)
                        .find(|&other| declares(other))
                };
                Some(InlineScope {
                    owner,
                    node,
                    direct,
                    parent_lambda,
                    foreign: ir.foreign_inline_templates.contains(&owner),
                })
            })
    }

    /// The inline declaration that owns the source implementation copied into `lambda`.
    pub(super) fn specialization_scope(&self, ir: &IrFile, lambda: FunId) -> Option<InlineScope> {
        let source = super::specialization_source(ir, lambda);
        (source != lambda)
            .then(|| self.inline_scope(ir, source))
            .flatten()
    }

    /// The inline function that `scope` is, or is nested in.
    fn inline_owner(
        &self,
        ir: &IrFile,
        scope: IrEnclosure,
        visited: &mut HashSet<FunId>,
    ) -> Option<FunId> {
        match scope {
            IrEnclosure::Function(function) | IrEnclosure::Lambda(function) => {
                if ir.inline_fns.contains(&function) {
                    return Some(function);
                }
                if !visited.insert(function) {
                    return None;
                }
                if let Some(scopes) = self.lambda_scopes.get(&function) {
                    return scopes
                        .iter()
                        .find_map(|&(_, scope)| self.inline_owner(ir, scope, visited));
                }
                let class = *self.method_owners.get(&function)?;
                self.inline_owner(ir, class_scope(ir, class)?, visited)
            }
            IrEnclosure::Constructor { class, .. }
            | IrEnclosure::ClassInitializer(class)
            | IrEnclosure::Classifier(class) => {
                self.inline_owner(ir, class_scope(ir, class)?, visited)
            }
            IrEnclosure::PropertyAccessor { .. } | IrEnclosure::File => None,
        }
    }
}

/// The scope a local or anonymous class is declared in; a named class declares none.
fn class_scope(ir: &IrFile, class: ClassId) -> Option<IrEnclosure> {
    ir.classes.get(class as usize)?.enclosure
}
