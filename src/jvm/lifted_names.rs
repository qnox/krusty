//! kotlinc's names for the methods it lifts lambdas and local functions into.
//!
//! `LocalDeclarationsLowering` names a lifted callable after the declarations around it, joined by
//! `$`: the outermost declaration (`outer`, `_init_` for constructors and initializers, `_get_x_`
//! for an accessor), then one segment per enclosing local callable, then its own. A lambda's
//! segment is `lambda$N`; a local function's is its name, or `name$N` when that name is already
//! taken in the sequence. `N` counts the lambdas and name-clashing local functions of the sequence
//! in source order, including lambdas that are spliced at inline call sites and so never become a
//! method. A suspend lambda becomes a class of its own and takes no number.
//! An inline declaration's delegated-property helper uses `lambda-N`; ordinary delegated locals
//! keep the same numbering and segment shape as other lambdas.
//!
//! Every lambda numbers what it contains on its own, local functions nested in it included, and
//! spells a lambda among them as the bare number: `one$lambda$0$0`, `one$lambda$0$lf$1`. A local
//! function does not: a lambda in `loc` takes the enclosing count, `one$loc$lambda$2`.
//!
//! A suspend lambda is no lifted method but a class of its own, whose `invokeSuspend` its body
//! becomes before kotlinc lifts anything out of it. What it declares is lifted into that class and
//! named after `invokeSuspend`, counted afresh: `invokeSuspend$lambda$0`, `invokeSuspend$local`,
//! `invokeSuspend$lambda$0$0`. Those names hold only once the lambda is realized as its class, so
//! they are kept apart ([`LambdaClassScopedNames`]) until [`realize`] knows that. An ordinary
//! `-Xlambdas=class` lambda likewise owns declarations under `invoke`.
//!
//! kotlinc renames a function whose signature mentions a value class before it lifts anything out
//! of it, so its JVM name is the outermost segment: `lamP_txdesME$lambda$0` in `lamP-txdesME`,
//! `m_impl$lambda$0` in a value-class member, `constructor_impl$lambda$0` in a value class's `init`
//! block. The sequence is that name's, so `o(Tag)` numbers apart from an overload `o(Int)`.

use crate::fx_hash::{HashMap, HashSet};

use crate::ir::{IrEnclosure, IrFile, IrLiftingRoot, IrLiftingSequence};

/// Lifted names of declarations a realized lambda class owns, each with that lambda's function.
/// The names apply only when the declaration actually moved into that exact class.
#[derive(Default)]
pub(crate) struct LambdaClassScopedNames {
    names: HashMap<crate::ir::FunId, (crate::ir::FunId, String)>,
}

/// Final JVM implementation ordinals for outermost lambdas moved into another physical class.
/// The placement pass records the exact function identity after allocating its new ordinal; lifted
/// naming then applies that ordinal to the matching source path and everything nested beneath it.
#[derive(Default)]
pub(super) struct PhysicalTopLambdaOrdinals {
    ordinals: HashMap<crate::ir::FunId, u32>,
}

impl PhysicalTopLambdaOrdinals {
    pub(super) fn record_moved(&mut self, ir: &IrFile, functions: &[crate::ir::FunId]) {
        let roots = functions
            .iter()
            .filter_map(|&function| {
                let (sequence, site) = ir.lifted_functions.get(&function)?;
                if !site.lifted {
                    return None;
                }
                let [step] = site.path.as_ref() else {
                    return None;
                };
                if step.kind != crate::lifting_provenance::LiftingCallableKind::Lambda {
                    return None;
                }
                let origin = ir.lambda_origins.get(&function)?;
                Some((
                    (sequence.clone(), step.position),
                    origin.implementation_ordinal,
                ))
            })
            .collect::<HashMap<_, _>>();
        for &function in functions {
            let Some(root) = lifting_root(ir, function) else {
                continue;
            };
            let Some(&ordinal) = roots.get(&(root.sequence, root.position)) else {
                continue;
            };
            self.ordinals.insert(function, ordinal);
        }
    }
}

/// Number every lifting sequence of the file and record kotlinc's lifted name of each function
/// lowered from a lambda or local function. A callable inside a suspend lambda, which is not
/// lifted, is named within that lambda's class and returned apart.
pub(super) fn number(
    ir: &mut IrFile,
    helper_access: &super::local_delegate_accessors::HelperAccess,
    physical_top_lambda_ordinals: &PhysicalTopLambdaOrdinals,
) -> LambdaClassScopedNames {
    // Lambda scopes that take no place among the lifted callables, by stable sequence position.
    // Suspend lambda scopes are already recorded in lifting provenance; ordinary class-mode
    // lambdas retain their consumed scope on the realized class.
    let mut class_scopes = ir
        .lifted_functions
        .iter()
        .filter_map(|(&function, (sequence, site))| {
            let step = site.path.last()?;
            (step.kind == crate::lifting_provenance::LiftingCallableKind::Lambda && !site.lifted)
                .then_some((
                    (sequence.clone(), step.position),
                    (function, "invokeSuspend"),
                ))
        })
        .collect::<HashMap<_, _>>();
    class_scopes.extend(ir.classes.iter().filter_map(|class| {
        let lambda = class.lambda.as_ref()?;
        let scope = lambda.lifting_scope.as_ref()?;
        Some((
            (scope.sequence.clone(), scope.position),
            (lambda.invoke, "invoke"),
        ))
    }));
    let inline_delegate_sites = ir
        .lifted_functions
        .iter()
        .filter(|(function, _)| helper_access.uses_inline_delegate_name(**function))
        .filter_map(|(_, (sequence, site))| site.path.last().map(|step| (sequence, step.position)))
        .collect::<HashSet<_>>();
    let mut segments = HashMap::<(&IrLiftingSequence, u32), String>::default();
    let mut containers = HashMap::<(&IrLiftingSequence, u32), String>::default();
    for (sequence, entries) in &ir.lifting_sequences {
        let mut scopes = HashMap::<(String, Option<u32>), (u32, HashSet<&str>)>::default();
        // kotlinc's value-class lowering appends the primary `constructor-impl`, which runs the
        // `init` blocks, after the class's other declarations, so what it lifts numbers after what
        // a secondary constructor's `constructor-impl` lifts. `order` is source order otherwise,
        // including a SAM adapter inserted between two source callables.
        let mut ordered = entries.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|(&position, entry)| {
            (
                in_primary_value_class_constructor(ir, entry.container),
                entry.order,
                position,
            )
        });
        for (&position, entry) in ordered {
            let container = container_segment(ir, entry.container, &sequence.container);
            containers.insert((sequence, position), container.clone());
            if !entry.lifted {
                continue;
            }
            let (next, used) = scopes.entry((container, entry.scope)).or_default();
            let segment = match entry.kind {
                crate::lifting_provenance::LiftingCallableKind::Lambda
                | crate::lifting_provenance::LiftingCallableKind::LocalDelegatedPropertyAccessor => {
                    assert!(
                        entry.name.is_none(),
                        "an unnamed lifting role gained a spelling"
                    );
                    let inline_delegate = entry.kind
                        == crate::lifting_provenance::LiftingCallableKind::LocalDelegatedPropertyAccessor
                        && inline_delegate_sites.contains(&(sequence, position));
                    // A suspend lambda's class numbers its callables as a declaration does.
                    let scope = entry
                        .scope
                        .filter(|&scope| !class_scopes.contains_key(&(sequence.clone(), scope)));
                    unnamed_segment(scope, inline_delegate, next)
                }
                crate::lifting_provenance::LiftingCallableKind::LocalFunction => {
                    let name = entry
                        .name
                        .as_deref()
                        .expect("a source local function retains its spelling");
                    if !used.insert(name) {
                        format!("{name}${}", take(next))
                    } else {
                        name.to_string()
                    }
                }
            };
            segments.insert((sequence, position), segment);
        }
    }
    let mut names = HashMap::default();
    let mut scoped = LambdaClassScopedNames::default();
    for (&function, (sequence, site)) in &ir.lifted_functions {
        let enclosing = site.path.split_last().map(|(_, enclosing)| enclosing);
        let lambda_class = enclosing.and_then(|enclosing| {
            enclosing
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, step)| {
                    let &(lambda, method) = class_scopes.get(&(sequence.clone(), step.position))?;
                    Some((index, lambda, method))
                })
        });
        let (mut name, start) = match (lambda_class, helper_access.lambda_path_start(function)) {
            (Some((index, _, method)), _) => (method.to_owned(), index + 1),
            (None, Some(start)) => ("invoke".to_owned(), start),
            (None, None) => (
                site.path
                    .first()
                    .and_then(|step| containers.get(&(sequence, step.position)))
                    .cloned()
                    .unwrap_or_else(|| segment_spelling(&site.container)),
                0,
            ),
        };
        let physical_top_ordinal = (start == 0)
            .then(|| physical_top_lambda_ordinals.ordinals.get(&function))
            .flatten();
        let spelled = site.path[start..].iter().enumerate().all(|(index, step)| {
            let Some(segment) = segments.get(&(sequence, step.position)) else {
                return false;
            };
            name.push('$');
            match (index, physical_top_ordinal) {
                (0, Some(ordinal)) => name.push_str(&format!("lambda${ordinal}")),
                _ => name.push_str(segment),
            }
            true
        });
        match (spelled, lambda_class) {
            (false, _) => {}
            (true, Some((_, lambda, _))) => {
                scoped.names.insert(function, (lambda, name));
            }
            (true, None) => {
                names.insert(function, name);
            }
        }
    }
    ir.lifted_names = names;
    scoped
}

/// Rename every function [`number`] named to its lifted name, once the functions are placed. One
/// whose lifted name and descriptor another method of the same class already has keeps its name:
/// kotlinc's names are distinct within the class it places them in, and a callable placed
/// elsewhere must not collide. A callable a suspend lambda declares takes the name its class gives
/// it when it is a method of that class, the lambda realized as kotlinc's `SuspendLambda`.
pub(crate) fn realize(
    ir: &mut IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    scoped: &LambdaClassScopedNames,
    machines: &crate::jvm::suspend::EmitTimeMachines,
) {
    let mut renames = ir.lifted_names.clone();
    for (&function, (lambda, name)) in &scoped.names {
        let in_lambda_class = ir.classes.iter().any(|class| {
            class.methods.contains(&function)
                && (class
                    .lambda
                    .as_ref()
                    .is_some_and(|realized| realized.invoke == *lambda)
                    || machines
                        .suspend_lambda(class.fq_name_id())
                        .is_some_and(|realized| realized.invoke_suspend == *lambda))
        });
        if in_lambda_class {
            ir.lifted_names.insert(function, name.clone());
            renames.insert(function, name.clone());
        }
    }
    let owners = ir
        .classes
        .iter()
        .enumerate()
        .flat_map(|(class, declaration)| {
            declaration
                .methods
                .iter()
                .map(move |&method| (method, class))
        })
        .collect::<HashMap<_, _>>();
    let descriptors = (0..ir.functions.len())
        .map(|function| super::ir_emit::function_descriptor(ir, override_results, function as u32))
        .collect::<Vec<_>>();
    loop {
        let mut holders = HashMap::<(Option<usize>, &str, &str), Vec<u32>>::default();
        for (index, function) in ir.functions.iter().enumerate() {
            let id = index as u32;
            // Foreign inline templates and typed helper prototypes have a declaration owner in
            // another file. They cannot collide with members emitted by this facade.
            if ir.inline_only_fns.contains(&id) {
                continue;
            }
            let name = renames
                .get(&id)
                .map_or(function.name.as_str(), String::as_str);
            holders
                .entry((owners.get(&id).copied(), name, &descriptors[index]))
                .or_default()
                .push(id);
        }
        let clashing = holders
            .into_values()
            .filter(|holders| holders.len() > 1)
            .flatten()
            .filter(|function| renames.contains_key(function))
            .collect::<Vec<_>>();
        if clashing.is_empty() {
            break;
        }
        for function in clashing {
            renames.remove(&function);
        }
    }
    for (function, name) in renames {
        ir.functions[function as usize].name = name;
    }
}

/// The outermost declaration's name as the first segment: the JVM name the value-class pass gave
/// `container`'s function, else the source `name`.
fn container_segment(ir: &IrFile, container: Option<IrEnclosure>, name: &str) -> String {
    let renamed = container
        .and_then(|container| container_function(ir, container))
        .filter(|function| ir.value_class_renamed_functions.contains(function))
        .map(|function| ir.functions[function as usize].name.as_str());
    segment_spelling(renamed.unwrap_or(name))
}

/// Whether `container` is the primary `constructor-impl` of a value class.
fn in_primary_value_class_constructor(ir: &IrFile, container: Option<IrEnclosure>) -> bool {
    container
        .and_then(|container| container_function(ir, container))
        .and_then(|function| ir.jvm_value_class_constructor_impls.get(&function))
        .is_some_and(|&ordinal| ordinal == 0)
}

/// The function realizing a lifting container that is a function body or a property accessor.
fn container_function(ir: &IrFile, container: IrEnclosure) -> Option<crate::ir::FunId> {
    match container {
        IrEnclosure::Function(function) => Some(function),
        IrEnclosure::PropertyAccessor { property, setter } => {
            super::ir_emit::enclosure::realized_property_accessor(ir, property, setter)
        }
        _ => None,
    }
}

/// The function whose body the lifted callable `function` is written in, through every enclosing
/// local callable: the outermost declaration of its lifting site, as the target realized it (a
/// value-class member's `-impl`, the `constructor-impl` running an `init` block). `None` for a
/// callable that is not lifted, or whose outermost declaration is no function body.
pub(super) fn root_container(ir: &IrFile, function: crate::ir::FunId) -> Option<crate::ir::FunId> {
    root_container_at(ir, &lifting_root(ir, function)?)
}

/// The outermost position of the path `function` is lifted along, which a lambda realized as a
/// class keeps after it stops being lifted.
pub(super) fn lifting_root(ir: &IrFile, function: crate::ir::FunId) -> Option<IrLiftingRoot> {
    let (sequence, site) = ir.lifted_functions.get(&function)?;
    Some(IrLiftingRoot {
        sequence: sequence.clone(),
        position: site.path.first()?.position,
    })
}

/// The exact innermost lambda step a realized class owns.
pub(super) fn lifting_scope(ir: &IrFile, function: crate::ir::FunId) -> Option<IrLiftingRoot> {
    let (sequence, site) = ir.lifted_functions.get(&function)?;
    Some(IrLiftingRoot {
        sequence: sequence.clone(),
        position: site.path.last()?.position,
    })
}

/// [`root_container`] from a recorded lifting root.
pub(super) fn root_container_at(ir: &IrFile, root: &IrLiftingRoot) -> Option<crate::ir::FunId> {
    let entry = ir
        .lifting_sequences
        .get(&root.sequence)?
        .get(&root.position)?;
    container_function(ir, entry.container?)
}

/// The function whose body a class declared in `enclosure` is written in, through every enclosing
/// lambda and local function, as the target realized it: the [`root_container`] of a local
/// function or suspend lambda, else the function or accessor itself. A suspend lambda realized as
/// a class of its own encloses what it declares in its `invokeSuspend`, but that class is no
/// source declaration, so nothing in it captures its instance: the root is the one the lambda's
/// class is written in. `None` for an enclosure that is no function body.
pub(super) fn enclosure_root(ir: &IrFile, enclosure: IrEnclosure) -> Option<crate::ir::FunId> {
    match enclosure {
        IrEnclosure::Function(function) => match root_container(ir, function) {
            Some(root) => Some(root),
            None => match generated_class_enclosure(ir, function) {
                Some(outer) => enclosure_root(ir, outer),
                None => Some(function),
            },
        },
        IrEnclosure::Lambda(lambda) => root_container(ir, lambda)
            .or_else(|| enclosure_root(ir, *ir.lambda_enclosures.get(&lambda)?)),
        _ => container_function(ir, enclosure),
    }
}

/// The function among `container` and the members of the local or anonymous classes it is written
/// in, outward, that is a member of `classifier`: where an instance of `classifier` captured in
/// `container` comes from. `None` when no class in that chain is `classifier`.
pub(super) fn classifier_member(
    ir: &IrFile,
    container: crate::ir::FunId,
    classifier: crate::types::TypeName,
) -> Option<crate::ir::FunId> {
    let mut function = container;
    loop {
        let owner = member_owner(ir, function)?;
        if owner.fq_name == classifier {
            return Some(function);
        }
        function = enclosure_root(ir, owner.enclosure?)?;
    }
}

/// The class `function` is a member of, a static realization of a member included.
fn member_owner(ir: &IrFile, function: crate::ir::FunId) -> Option<&crate::ir::IrClass> {
    match ir.functions[function as usize].dispatch_receiver {
        Some(owner) => Some(&ir.classes[ir.class_id_by_name(owner)? as usize]),
        None => ir
            .classes
            .iter()
            .find(|class| class.methods.contains(&function)),
    }
}

/// Where the compiler-generated class whose method `function` is, such as a suspend lambda's class
/// owning its `invokeSuspend`, is declared; `None` for a method of a source class.
fn generated_class_enclosure(ir: &IrFile, function: crate::ir::FunId) -> Option<IrEnclosure> {
    let owner = ir.functions[function as usize].dispatch_receiver?;
    let class = &ir.classes[ir.class_id_by_name(owner)? as usize];
    match class.is_source_declared {
        true => None,
        false => class.enclosure,
    }
}

/// kotlinc replaces the characters a special or mangled name carries (`<init>`, `<get-x>`,
/// `x$delegate`, `f-txdesME`) with `_`.
fn segment_spelling(container: &str) -> String {
    container
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | '-' | '$') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// The next number of a scope's counter.
fn take(next: &mut u32) -> u32 {
    *next += 1;
    *next - 1
}

fn unnamed_segment(scope: Option<u32>, inline_delegate: bool, next: &mut u32) -> String {
    let number = take(next);
    if inline_delegate {
        format!("lambda-{number}")
    } else if scope.is_some() {
        number.to_string()
    } else {
        format!("lambda${number}")
    }
}

#[cfg(test)]
mod tests {
    use super::{segment_spelling, unnamed_segment};

    #[test]
    fn ordinary_unnamed_callables_and_inline_delegates_keep_distinct_segments() {
        let mut next = 0;
        assert_eq!(unnamed_segment(None, false, &mut next), "lambda$0");
        assert_eq!(unnamed_segment(Some(0), false, &mut next), "1");
        assert_eq!(unnamed_segment(None, true, &mut next), "lambda-2");
        assert_eq!(unnamed_segment(Some(0), true, &mut next), "lambda-3");
        assert_eq!(next, 4);
    }

    #[test]
    fn special_container_names_take_underscores() {
        assert_eq!(segment_spelling("<init>"), "_init_");
        assert_eq!(segment_spelling("<get-top>"), "_get_top_");
        assert_eq!(segment_spelling("lz$delegate"), "lz_delegate");
        assert_eq!(segment_spelling("sus-47cE-Rs"), "sus_47cE_Rs");
        assert_eq!(segment_spelling("outer"), "outer");
    }
}
