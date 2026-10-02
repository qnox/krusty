//! Where a lambda or local function sits among the callables its declaration lifts.

/// Where a lambda or local function sits among the callables kotlinc lifts out of one declaration:
/// the sequence (its lexical `owner` and outermost declaration name `container`, as the source
/// spells them) and one step per enclosing local callable, down to this one. `lifted` is `false`
/// for a callable kotlinc turns into a class of its own (a suspend lambda), which takes no place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirLiftingSite {
    pub owner: Box<str>,
    pub container: Box<str>,
    pub path: Box<[FirLiftingStep]>,
    pub lifted: bool,
}

/// One enclosing local callable of a [`FirLiftingSite`]: its semantic role, optional source name,
/// and position in the sequence's source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirLiftingStep {
    pub kind: crate::lifting_provenance::LiftingCallableKind,
    pub name: Option<Box<str>>,
    pub position: u32,
}

impl FirLiftingSite {
    pub fn from_source(site: &crate::ast::LiftingSite, lifted: bool) -> Self {
        Self {
            owner: site.owner.as_str().into(),
            container: site.container.as_str().into(),
            path: site
                .path
                .iter()
                .map(|step| FirLiftingStep {
                    kind: step.kind,
                    name: step.name.as_deref().map(Into::into),
                    position: step.position,
                })
                .collect(),
            lifted,
        }
    }
}
