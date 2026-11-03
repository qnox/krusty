# How facts travel between phases

The checker, common lowering and each backend hand facts to the next phase. Today most of those
facts travel in sparse side tables: `TypeInfo` holds about 70 maps keyed by an AST expression or a
source span, and `IrFile` holds about 150 tables keyed by an expression or function id. A table is
a promise every later transform has to keep. When a pass clones, wraps, relocates or specializes an
expression, it must remember every table that names that expression, and a forgotten one drops the
fact without any error. That is where most "Preserve X" and "Keep Y" fixes come from.

This document says where a fact lives instead, and how the existing tables migrate.

## Where a fact lives

1. **Node facts live on the node.** Every common-IR expression has a source position (start and
   end), a checked type and an origin, the way kotlinc's `IrElement` carries `startOffset`,
   `endOffset`, `type` and `origin`. `IrFile::add_expr` writes them with the node, so a node
   without them cannot be built, and a structural copy carries them because it copies the node.
   The roles lowering records today as membership sets (`safe_calls`, `null_guards`,
   `negations`, `short_circuits`, `postfix_updates`, `written_casts`, the `when` and elvis
   provenance, and so on) are values of one origin enum on the node, like kotlinc's
   `IrStatementOrigin`.
2. **Operation facts live in the operation's payload.** What the checker selected for a call,
   construction or property operation (declared parameters and result, dispatch class, inline
   modifiers, reified and inline type arguments, suspend result, the selected constructor) is part
   of that `IrExpr` variant. A representation wrapper that moves the operation below itself moves
   the payload with it, so nothing has to list which facts "identify the operation".
3. **Declaration facts live on the declaration.** Facts keyed by a function id (`fn_*`,
   `lambda_*`, `*_fns`, `signatures`) belong on `IrFunction`, and class facts on `IrClass`.
4. **Backend realization facts live in the backend.** A table a backend writes for its own physical
   representation (the `jvm_*` and value-class realization tables) has an owning backend module
   and moves out of `IrFile` into that backend's realization state. It is the only kind of side
   table that remains, and it declares how a copy treats it.
5. **The checker builds the checked tree.** The FIR builder walks the AST a second time and reads
   the checker's decisions back from `TypeInfo`. The target is that the checker writes checked FIR
   nodes with their selected callable and type directly, file by file, and `TypeInfo`'s per-node
   maps disappear with that rebuild. This is the frontend half of the same rule and lands with the
   resolution-engine work (`docs/RESOLUTION_ENGINE.md`).

Sparse tables are still right for facts that genuinely exist for few nodes and belong to one
owner. They are wrong for facts every node of a kind has, and for facts that must survive copies.

## The guard: every `IrFile` field is classified

`src/ir/expression_copy.rs` destructures `IrFile` without a rest pattern. Each field is named once,
as a fact a copy of an expression inherits, as an expression fact a copy does not inherit (with
the reason), or as a file, declaration or class table. A new field does not compile until its
author makes that decision, and `deny(unused_variables)` fails the build when a field named as
inherited is not copied. A field that holds tables of its own (`GeneratedLineMarks`,
`InlineExpansions`, `IrWhenFacts`, …) implements `InheritedFacts` in its owning module with the
same exhaustive destructuring. `TypeInfo` needs no counterpart: nothing clones or renumbers AST
expressions, so its failure mode is the read-back seam in item 5, not a forgotten copy.

The classification also documents what remains to migrate: every table in the inherited group is
a node, operation or declaration fact by the rule above.

## Migration order

Each step is a small PR that moves one table family onto nodes or payloads, deletes the tables and
every hand-written copy of them, and keeps box and the corpus green. The order follows how often a
family was patched in the 30 days before 1 Nov 2026 (commits touching it under `src/`):

| Step | Family | Tables | Commits |
|------|--------|--------|---------|
| 1 | Source positions | `expr_source_lines`, `expr_end_lines`, `expr_lines`, `kept_start_lines`, `positionless` and the other generated line marks | 64 |
| 2 | Checked types | `logical_types`, `physical_types`, `deferred_local_types` | 77 |
| 3 | Call selections | `call_declared_ret`, `call_declared_params`, `dispatch_classes`, `ext_call_source_receiver`, `reified_call_subst`, `inline_call_type_arguments`, `call_inline_modifiers`, `construction_*`, `suspend_calls`, … | 44 |
| 4 | Origins and roles | `fir_origins`, the role sets above, `whens` | 47 |
| 5 | Declaration facts | the `fn_*`, `lambda_*` and `*_fns` tables | — |

Step 1 is split in two. First, `expr_source_lines` and `expr_end_lines` become one
`IrNodePosition` per node (`src/ir/node_positions.rs`), read and written through `IrFile`
accessors and copied as part of the node by every copy site. Behavior is unchanged: a node has the
same lines it had in the maps. Second, lowering gives every node it builds for a FIR expression
that expression's position, as fir2ir does, and the JVM backend decides by node kind where a line
begins instead of by which nodes happen to carry a line. That second half changes line tables, so
it lands together with the JVM line-marking work.

Positions come first although types were patched more often, because positions are what the JVM
line-number work is waiting on: once every node has offsets, kotlinc's line-marking rule is ported
once instead of per construct. Each step also deletes its tables from the other hand-written copy
sites (`jvm/operation_relocation.rs`, `jvm/external_calls/arguments.rs`, `jvm/ranges.rs`,
`fir_lower/inlining::specialize_recorded_facts`), which today each keep their own list.
