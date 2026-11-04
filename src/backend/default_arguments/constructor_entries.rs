//! The default-argument entry of a constructor: one more constructor of its class, taking the
//! constructor's whole frame and the omission masks, which fills each omitted parameter from its
//! default and then delegates `this(…)` to the constructor.
//!
//! A construction allocates before any constructor runs, and a delegation runs on an object that
//! already exists, so the entry is a constructor rather than a function. For
//! `class C(val a: Int, val b: Int = a + 1)` it is, in effect,
//! `constructor(a: Int, b: Int, mask: Int) : this(a, if (mask and 2 != 0) a + 1 else b)`: the
//! fills run as the delegation's prelude, in the constructor's own frame, so a default reads the
//! parameters before it as the call supplied or filled them.
//!
//! Its parameters are the compiler-supplied prefix every constructor of the class takes, then the
//! source parameters, then one `Int` mask word per 32 of them. A mask word can make that list
//! equal to one the class declares, so the entry is reached by its exact place among the class's
//! constructors, never by its parameter list.

use crate::ir::{
    ClassId, CtorDelegateTarget, ExprId, IrConstructorTarget, IrFile, IrSecondaryCtor,
};
use crate::types::Ty;

use super::entries::{fills, read};
use super::masks::mask_words;

/// What a default-argument call needs to know of one declared constructor.
pub(super) struct DeclaredConstructor {
    /// The compiler-supplied leading parameters: an enclosing instance, a local class's captures.
    /// Nothing omits them.
    pub(super) prefix: Vec<Ty>,
    /// The source parameters, which default-argument ordinals and masks count.
    pub(super) parameters: Vec<Ty>,
    /// Each source parameter's default, parallel to `parameters`.
    pub(super) defaults: Vec<Option<ExprId>>,
}

impl DeclaredConstructor {
    fn declares_defaults(&self) -> bool {
        self.defaults.iter().any(Option::is_some)
    }
}

/// Constructor `ordinal` of `class`: 0 for the primary, `n` for the `n`th secondary.
pub(super) fn declared(ir: &IrFile, class: ClassId, ordinal: u32) -> Option<DeclaredConstructor> {
    let declaration = &ir.classes[class as usize];
    let Some(secondary) = ordinal.checked_sub(1) else {
        if !declaration.has_primary_ctor {
            return None;
        }
        let prefix = usize::try_from(declaration.constructor_prefix_count).ok()?;
        let (leading, own) = declaration.ctor_args.split_at_checked(prefix)?;
        // The primary's defaults are recorded beside the class, parallel to all its parameters.
        let defaults = match ir.class_ctor_defaults_name(declaration.fq_name_id()) {
            Some(defaults) if defaults.len() == declaration.ctor_args.len() => {
                defaults[prefix..].to_vec()
            }
            Some(_) => return None,
            None => vec![None; own.len()],
        };
        return Some(DeclaredConstructor {
            prefix: leading.iter().map(|argument| argument.ty).collect(),
            parameters: own.iter().map(|argument| argument.ty).collect(),
            defaults,
        });
    };
    let constructor = declaration.secondary_ctors.get(secondary as usize)?;
    (constructor.defaults.len() == constructor.params.len()).then(|| DeclaredConstructor {
        prefix: constructor.prefix_params.clone(),
        parameters: constructor.params.clone(),
        defaults: constructor.defaults.clone(),
    })
}

/// Build the entry of the constructor `target` of `class` selects, when it declares defaults.
/// Answers how the entry is reached, with the roots of its delegation for the call sites in it.
pub(super) fn build(
    ir: &mut IrFile,
    class: ClassId,
    target: IrConstructorTarget,
    clones: &mut Vec<(ExprId, ExprId)>,
) -> Option<(IrConstructorTarget, Vec<ExprId>)> {
    let declared = declared(ir, class, target.ordinal)?;
    if !declared.declares_defaults() {
        return None;
    }
    // `this` is value 0; the prefix and then the source parameters follow it.
    let first = 1 + declared.prefix.len();
    let parameters = declared.parameters.len();
    let prelude = fills(ir, &declared.defaults, first, parameters, clones);
    let arguments = declared
        .parameters
        .iter()
        .enumerate()
        .map(|(ordinal, &ty)| {
            read(
                ir,
                u32::try_from(first + ordinal).expect("too many parameters"),
                ty,
            )
        })
        .collect::<Vec<_>>();
    let mut roots = prelude.clone();
    roots.extend(&arguments);
    let params = declared
        .parameters
        .iter()
        .copied()
        .chain(std::iter::repeat_n(Ty::Int, mask_words(parameters)))
        .collect::<Vec<_>>();
    let secondaries = &mut ir.classes[class as usize].secondary_ctors;
    secondaries.push(IrSecondaryCtor {
        annotations: Default::default(),
        // Compiler-generated: no source position and no Kotlin declaration of its own.
        source_order: u32::MAX,
        lines: Default::default(),
        prefix_params: declared.prefix,
        defaults: vec![None; params.len()],
        params,
        declared_spellings: Default::default(),
        named_params: Vec::new(),
        metadata_visibility: None,
        generated_debug: Default::default(),
        vararg_index: None,
        delegate_prelude: prelude,
        delegate_args: arguments,
        default_parameters: Vec::new(),
        body: None,
        delegate: CtorDelegateTarget::This {
            target_params: declared.parameters,
            target,
            default_masks: Vec::new(),
        },
        synthetic: true,
        vc_params: false,
        param_checks: Vec::new(),
    });
    let entry = IrConstructorTarget {
        ordinal: u32::try_from(secondaries.len()).expect("too many constructors"),
        // Whoever may call the constructor may call its entry.
        access: target.access,
    };
    Some((entry, roots))
}
