//! JVM emission facts produced while coroutine lowering still owns the physical suspend shape.

/// JVM class metadata computed before continuation spill scopes are discarded.
#[derive(Clone, Debug, Default)]
pub struct ContinuationMetadata {
    pub l: Vec<i32>,
    pub nl: Vec<i32>,
    pub i: Vec<i32>,
    pub s: Vec<String>,
    pub n: Vec<String>,
    pub m: String,
    pub c: String,
    pub v: i32,
    pub enclosing_class: String,
    pub enclosing_method: String,
    pub enclosing_descriptor: String,
}

pub type ContinuationMetadataMap = std::collections::HashMap<String, ContinuationMetadata>;

/// JVM return nodes whose forwarded suspend result preserves `COROUTINE_SUSPENDED` but otherwise
/// becomes `Unit.INSTANCE` for the selected Kotlin target version.
pub(crate) type UnitResultTailForwards = std::collections::HashSet<crate::ir::ExprId>;
