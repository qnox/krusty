//! The applier scheme kotlinc's `ComposableTargetAnnotationsTransformer` infers for a composable
//! function that takes composable lambdas, which it records as `@ComposableInferredTarget`.
//!
//! A scheme names the applier the function's own calls need and the one each composable lambda
//! parameter's calls need. Calling a lambda parameter, or passing it to a function whose scheme
//! binds that parameter to its own applier, makes the lambda need the caller's applier. Appliers
//! that must be the same share a number (`[0[0][_]]`); one nothing constrains is open (`_`). The
//! pass compiles only calls within one file and no explicit target, so every applier is a
//! variable and the inference is unification over them.

use crate::fx_hash::HashMap;
use crate::ir::FunId;

/// What a function's body does with its composable lambda parameters.
pub(super) struct TargetConstraints {
    /// The function's composable lambda parameters, by parameter index, in order.
    pub(super) lambdas: Vec<u32>,
    /// The lambda parameters the body calls.
    pub(super) invoked: Vec<u32>,
    /// The composable calls the body passes lambda parameters to.
    pub(super) calls: Vec<LambdaCall>,
}

/// A composable call of a function of the same file that is passed lambda parameters.
pub(super) struct LambdaCall {
    pub(super) callee: FunId,
    /// Each passed lambda: the callee's parameter it is passed to, and the caller's own parameter.
    pub(super) arguments: Vec<(u32, u32)>,
}

/// A function's scheme: its own applier, then each lambda parameter's, numbered by shared
/// applier, or `None` for an open one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Scheme {
    target: Option<u32>,
    parameters: Vec<Option<u32>>,
}

impl Scheme {
    /// The `@ComposableInferredTarget` scheme string, when inference bound anything; kotlinc
    /// annotates only a scheme it changed from the declared all-open one.
    pub(super) fn inferred(&self) -> Option<String> {
        if self.target.is_none() && self.parameters.iter().all(Option::is_none) {
            return None;
        }
        let item = |item: Option<u32>| item.map_or_else(|| "_".to_string(), |n| n.to_string());
        let parameters = self
            .parameters
            .iter()
            .map(|&parameter| format!("[{}]", item(parameter)))
            .collect::<String>();
        Some(format!("[{}{parameters}]", item(self.target)))
    }
}

/// The scheme of every function in `functions`, or the first function found to need its own
/// scheme while it is inferred (a recursive call).
pub(super) fn infer(
    functions: &HashMap<FunId, TargetConstraints>,
) -> Result<HashMap<FunId, Scheme>, FunId> {
    let mut schemes = HashMap::default();
    let mut order = functions.keys().copied().collect::<Vec<_>>();
    order.sort_unstable();
    for function in order {
        scheme_of(function, functions, &mut schemes, &mut Vec::new())?;
    }
    Ok(schemes)
}

fn scheme_of(
    function: FunId,
    functions: &HashMap<FunId, TargetConstraints>,
    schemes: &mut HashMap<FunId, Scheme>,
    inferring: &mut Vec<FunId>,
) -> Result<Scheme, FunId> {
    if let Some(scheme) = schemes.get(&function) {
        return Ok(scheme.clone());
    }
    if inferring.contains(&function) {
        return Err(function);
    }
    let constraints = &functions[&function];
    inferring.push(function);
    // Node 0 is the function's applier, node 1 + i its lambda `i`'s.
    let mut appliers = Appliers::new(1 + constraints.lambdas.len());
    let node = |parameter: u32| {
        1 + constraints
            .lambdas
            .iter()
            .position(|&lambda| lambda == parameter)
            .expect("a constrained parameter is a lambda")
    };
    for &parameter in &constraints.invoked {
        appliers.union(0, node(parameter));
    }
    for call in &constraints.calls {
        let callee = scheme_of(call.callee, functions, schemes, inferring)?;
        // A fresh instance of the callee's scheme: its applier is the caller's, and each lambda
        // passed needs what the callee binds that parameter to.
        let mut instance = HashMap::default();
        let mut instantiate = |appliers: &mut Appliers, number: u32| {
            *instance.entry(number).or_insert_with(|| appliers.fresh())
        };
        if let Some(target) = callee.target {
            let target = instantiate(&mut appliers, target);
            appliers.union(0, target);
        }
        for &(callee_parameter, parameter) in &call.arguments {
            let callee_lambda = functions[&call.callee]
                .lambdas
                .iter()
                .position(|&lambda| lambda == callee_parameter)
                .expect("a lambda is passed to a lambda parameter");
            if let Some(number) = callee.parameters[callee_lambda] {
                let bound = instantiate(&mut appliers, number);
                appliers.union(node(parameter), bound);
            }
        }
    }
    inferring.pop();
    let roots = (0..=constraints.lambdas.len())
        .map(|item| appliers.find(item))
        .collect::<Vec<_>>();
    let mut numbers = HashMap::default();
    let mut item = |root: usize| {
        if roots.iter().filter(|&&other| other == root).count() < 2 {
            return None;
        }
        let next = numbers.len() as u32;
        Some(*numbers.entry(root).or_insert(next))
    };
    let target = item(roots[0]);
    let parameters = roots[1..].iter().map(|&root| item(root)).collect();
    let scheme = Scheme { target, parameters };
    schemes.insert(function, scheme.clone());
    Ok(scheme)
}

/// Union-find over appliers.
struct Appliers {
    parents: Vec<usize>,
}

impl Appliers {
    fn new(count: usize) -> Self {
        Self {
            parents: (0..count).collect(),
        }
    }

    fn fresh(&mut self) -> usize {
        self.parents.push(self.parents.len());
        self.parents.len() - 1
    }

    fn find(&mut self, item: usize) -> usize {
        let parent = self.parents[item];
        if parent == item {
            return item;
        }
        let root = self.find(parent);
        self.parents[item] = root;
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parents[a.max(b)] = a.min(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constraints(
        lambdas: &[u32],
        invoked: &[u32],
        calls: &[(FunId, &[(u32, u32)])],
    ) -> TargetConstraints {
        TargetConstraints {
            lambdas: lambdas.to_vec(),
            invoked: invoked.to_vec(),
            calls: calls
                .iter()
                .map(|&(callee, arguments)| LambdaCall {
                    callee,
                    arguments: arguments.to_vec(),
                })
                .collect(),
        }
    }

    /// The schemes kotlinc 2.4.21 annotated these declarations with.
    #[test]
    fn schemes_match_kotlinc() {
        let functions = [
            // Box(content) { content() }
            (0, constraints(&[0], &[0], &[])),
            // P(content) {}
            (1, constraints(&[0], &[], &[])),
            // Q(a, b) { a(); b() }
            (2, constraints(&[0, 1], &[0, 1], &[])),
            // R(a, b) { a() }
            (3, constraints(&[0, 1], &[0], &[])),
            // S(a) { Box(a) }
            (4, constraints(&[0], &[], &[(0, &[(0, 0)])])),
            // W(a, b) { R(b, a) }
            (5, constraints(&[0, 1], &[], &[(3, &[(0, 1), (1, 0)])])),
            // U(a) { P(a) }
            (6, constraints(&[0], &[], &[(1, &[(0, 0)])])),
            // X(a, b) { R(b, a); a() }
            (7, constraints(&[0, 1], &[0], &[(3, &[(0, 1), (1, 0)])])),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>();
        let schemes = infer(&functions).expect("no recursion");
        let inferred = |function: FunId| schemes[&function].inferred();
        assert_eq!(inferred(0).as_deref(), Some("[0[0]]"));
        assert_eq!(inferred(1), None);
        assert_eq!(inferred(2).as_deref(), Some("[0[0][0]]"));
        assert_eq!(inferred(3).as_deref(), Some("[0[0][_]]"));
        assert_eq!(inferred(4).as_deref(), Some("[0[0]]"));
        assert_eq!(inferred(5).as_deref(), Some("[0[_][0]]"));
        assert_eq!(inferred(6), None);
        assert_eq!(inferred(7).as_deref(), Some("[0[0][0]]"));
    }

    #[test]
    fn a_recursive_call_is_reported() {
        let functions = [(0, constraints(&[0], &[], &[(0, &[(0, 0)])]))]
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(infer(&functions), Err(0));
    }
}
