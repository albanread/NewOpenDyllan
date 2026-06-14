//! Sprint 15 dispatch resolution — rewrite `Computation::Dispatch`
//! nodes to `Computation::DirectCall` (or `Computation::SealedDirectCall`)
//! when the sealing facts plus the type-estimate lattice permit.
//!
//! Algorithm per spec 15 §5:
//!
//!   For each Dispatch node:
//!     1. Look up the generic; skip if unknown.
//!     2. Read arg type estimates from the narrower's output.
//!     3. Check the closure condition — generic sealed OR estimates
//!        fall under a sealed domain OR every estimate's class is itself
//!        sealed. If no closure, leave as Dispatch.
//!     4. Enumerate applicable methods (`est[i] <: M.specialisers[i]`
//!        for all i). The "<:" predicate uses `TypeEstimate::is_subtype_of`
//!        threaded with the runtime's `is_subclass`.
//!     5. Sort by specificity using Sprint 13's CPL-driven rule. Unique
//!        winner → rewrite; ambiguous → leave as Dispatch.
//!     6. Emit `SealedDirectCall` (chain ≥ 1 less-specific applicable
//!        method) or `DirectCall` (single applicable method, no chain).
//!     7. Record the dependency in the runtime's
//!        `ResolvedDispatchIndex` for Sprint 29 invalidation.
//!
//! Soundness rule: when in doubt, leave as Dispatch. The fast path is
//! opt-in.

use std::sync::atomic::{AtomicU64, Ordering};

use nod_dfm::{Computation, Function, TempId, TypeEstimate};
use nod_runtime::{
    ClassId, GenericFunction, Method, ResolvedDispatchEntry, class_metadata_ptr, find_generic,
    is_subclass, record_resolved_dispatch,
};

use super::facts::SealingFacts;
use super::narrowing::NarrowedEstimates;

/// One outcome of running the resolver against a `Computation::Dispatch`.
/// Returned in a vector for diagnostic / dump-dispatch use.
#[derive(Clone, Debug)]
pub enum DispatchResolution {
    /// The Dispatch was rewritten. `call_site_id` is the unique
    /// identifier the resolver minted; `method_name` is the JIT'd
    /// body symbol it picked; `reason` describes the closure rule
    /// that justified the rewrite.
    SealedDirect {
        call_site_id: u64,
        generic_name: String,
        method_name: String,
        reason: String,
        chain_depth: usize,
    },
    /// The Dispatch survived. `reason` explains why (e.g. "no closure",
    /// "ambiguous", "generic unknown").
    LeftAsDispatch {
        call_site_id: u64,
        generic_name: String,
        reason: String,
    },
}

/// Mint a fresh resolver-side call-site id. Distinct from Sprint 13's
/// JIT-cache-slot site ids; this one tags resolved Dispatches in the
/// runtime's back-reference index so Sprint 29's invalidation cascade
/// has something to walk.
static NEXT_RESOLVER_SITE_ID: AtomicU64 = AtomicU64::new(1);

fn next_site_id() -> u64 {
    NEXT_RESOLVER_SITE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Run the dispatch resolution pass on a single function. Mutates
/// `f` in place — rewrites `Dispatch` nodes that pass the closure +
/// specificity checks to `DirectCall` (single applicable method) or
/// `SealedDirectCall` (single most-specific + non-empty fallback
/// chain).
pub fn resolve_dispatches(
    f: &mut Function,
    narrowed: &NarrowedEstimates,
    sealing: &SealingFacts,
) -> Vec<DispatchResolution> {
    let mut log: Vec<DispatchResolution> = Vec::new();
    for block in &mut f.blocks {
        for c in &mut block.computations {
            let Computation::Dispatch {
                dst,
                generic_name,
                args,
                safepoint_roots,
            } = c
            else {
                continue;
            };
            let outcome = resolve_one(
                generic_name,
                args,
                narrowed,
                sealing,
            );
            match outcome {
                Outcome::Rewrite {
                    chain,
                    reason,
                } => {
                    let site_id = next_site_id();
                    let chain_depth = chain.len() - 1;
                    let head = chain[0].clone();
                    let fallback: Vec<String> = chain[1..].to_vec();

                    // Spec 15 §9.6 + spec 15 §10 OQ6: preserve
                    // safepoint_roots verbatim. The rewrite swaps the
                    // call-shaped node but keeps the same roots field
                    // — Sprint 11b's GC contract applies identically.
                    let preserved_roots = std::mem::take(safepoint_roots);
                    let preserved_args = std::mem::take(args);
                    let preserved_dst = *dst;
                    let preserved_generic = generic_name.clone();

                    record_resolved_dispatch(ResolvedDispatchEntry {
                        call_site_id: site_id,
                        generic_name: preserved_generic.clone(),
                        recorded_generation: find_generic(&preserved_generic)
                            .map(|g| g.generation())
                            .unwrap_or(0),
                        resolved_method: head.clone(),
                    });

                    // Sprint 15: emit `DirectCall` (single applicable
                    // method, no chain) or `SealedDirectCall` (2+
                    // applicable methods, chain non-empty). The
                    // codegen layer handles both; SealedDirectCall
                    // adds a `nod_push_sealed_chain_frame` /
                    // `nod_pop_sealed_chain_frame` pair around the
                    // resolved method call so `next-method()` inside
                    // the body walks the fallback chain identically
                    // to the runtime dispatcher's behaviour.
                    if fallback.is_empty() {
                        *c = Computation::DirectCall {
                            dst: preserved_dst,
                            callee: head.clone(),
                            args: preserved_args,
                            safepoint_roots: preserved_roots,
                            // Sprint 48: dispatch resolver doesn't (yet)
                            // analyse user-method bodies for no_alloc.
                            // Phase B would propagate it from the
                            // resolved method's Function::no_alloc.
                            is_no_alloc: false,
                        };
                    } else {
                        *c = Computation::SealedDirectCall {
                            dst: preserved_dst,
                            method: head.clone(),
                            fallback_chain: fallback,
                            generic_name: preserved_generic.clone(),
                            args: preserved_args,
                            safepoint_roots: preserved_roots,
                            is_no_alloc: false,
                        };
                    }

                    log.push(DispatchResolution::SealedDirect {
                        call_site_id: site_id,
                        generic_name: preserved_generic,
                        method_name: head,
                        reason,
                        chain_depth,
                    });
                }
                Outcome::Leave { reason } => {
                    log.push(DispatchResolution::LeftAsDispatch {
                        call_site_id: next_site_id(),
                        generic_name: generic_name.clone(),
                        reason,
                    });
                }
            }
        }
    }
    log
}

enum Outcome {
    /// Rewrite to a DirectCall / SealedDirectCall. `chain` is the
    /// applicable-method body symbols in most-specific-first order
    /// (index 0 is the resolved head; the rest is the next-method
    /// chain). `reason` is a human-readable description of the
    /// closure justification.
    Rewrite {
        chain: Vec<String>,
        reason: String,
    },
    /// Leave the Dispatch alone. Sprint 13's inline cache picks up at
    /// runtime.
    Leave { reason: String },
}

fn resolve_one(
    generic_name: &str,
    args: &[TempId],
    narrowed: &NarrowedEstimates,
    sealing: &SealingFacts,
) -> Outcome {
    // 1. Look up the generic.
    let Some(generic) = find_generic(generic_name) else {
        return Outcome::Leave {
            reason: format!("generic `{generic_name}` not registered"),
        };
    };

    // 2. Read arg type estimates.
    let estimates: Vec<TypeEstimate> = args
        .iter()
        .map(|t| narrowed.get(t).copied().unwrap_or(TypeEstimate::Top))
        .collect();

    // 3. Check the closure condition.
    let closure = match check_closure(generic, &estimates, sealing) {
        Some(reason) => reason,
        None => {
            return Outcome::Leave {
                reason: "no closure (generic open + no sealed domain + receiver class open)"
                    .to_string(),
            };
        }
    };

    // 4. Enumerate applicable methods.
    let methods = generic.methods.read().expect("methods rwlock poisoned");
    let applicable_indices: Vec<usize> = methods
        .iter()
        .enumerate()
        .filter(|(_, m)| is_guaranteed_applicable(m, &estimates))
        .map(|(i, _)| i)
        .collect();
    if applicable_indices.is_empty() {
        return Outcome::Leave {
            reason: "no statically applicable method (would error at runtime)".to_string(),
        };
    }

    // 5. Sort by specificity using Sprint 13's CPL-driven rule.
    let mut sorted: Vec<usize> = applicable_indices;
    sorted.sort_by(|&a, &b| compare_specificity(&methods[a], &methods[b], &estimates));
    // Ambiguity check: if the top two compare as Equal, we cannot
    // pick a unique winner.
    if sorted.len() >= 2 {
        let cmp = compare_specificity(&methods[sorted[0]], &methods[sorted[1]], &estimates);
        if cmp == std::cmp::Ordering::Equal {
            return Outcome::Leave {
                reason: "ambiguous applicable methods (cannot pick most-specific)".to_string(),
            };
        }
    }

    // 6. Build the body-symbol chain. Sprint 13's `Method` carries a
    //    raw fn pointer; the body symbol name lives on the
    //    `MethodRegistration` keyed by `(generic_name, specialisers)`.
    //    We don't have direct access to the registration table here, so
    //    we reconstruct the symbol from the convention used by
    //    `lower_method_item`: `format!("{}${}", generic_name,
    //    specialisers.iter().map(|c| c.0.to_string()).join("_"))`.
    let chain: Vec<String> = sorted
        .iter()
        .map(|&i| method_body_symbol(generic_name, &methods[i]))
        .collect();

    Outcome::Rewrite {
        chain,
        reason: closure,
    }
}

fn method_body_symbol(generic_name: &str, m: &Method) -> String {
    // Sprint 16: prefer the method's registered `body_fn_name` when
    // present. Sprint 12's slot accessors don't follow the
    // `{generic}${specialisers}` convention — their bodies live at
    // `<C>-getter-<slot>` / `<C>-setter-<slot>` — so the resolver
    // can't reconstruct the symbol algorithmically. The
    // `add_method_named` API records the JIT symbol the codegen
    // actually produced; the resolver consults it here.
    if !m.body_fn_name.is_empty() {
        return m.body_fn_name.clone();
    }
    let suffix = m
        .specialisers
        .iter()
        .map(|c| c.0.to_string())
        .collect::<Vec<_>>()
        .join("_");
    format!("{generic_name}${suffix}")
}

/// Spec 15 §5.3 closure check. Returns `Some(reason)` if closure
/// holds, `None` if it doesn't. `reason` is a human-readable
/// description.
fn check_closure(
    generic: &GenericFunction,
    estimates: &[TypeEstimate],
    sealing: &SealingFacts,
) -> Option<String> {
    if generic.is_sealed() {
        return Some("generic is sealed".to_string());
    }
    if sealing.is_sealed_generic(&generic.name) {
        return Some("generic is sealed (per SealingFacts)".to_string());
    }
    // Sealed domain covers all estimates?
    let runtime_domains = generic.sealed_domains_snapshot();
    let fact_domains: Vec<Vec<ClassId>> = sealing
        .sealed_domains_for(&generic.name)
        .to_vec();
    for domain in runtime_domains.iter().chain(fact_domains.iter()) {
        if domain.len() != estimates.len() {
            continue;
        }
        if domain
            .iter()
            .zip(estimates.iter())
            .all(|(d, e)| e.is_subtype_of_class(d.0, &class_subclass_check))
        {
            return Some(format!(
                "sealed-domain covers estimates ({} specialiser{})",
                domain.len(),
                if domain.len() == 1 { "" } else { "s" }
            ));
        }
    }
    // Every estimate's class is itself sealed?
    if !estimates.is_empty()
        && estimates.iter().all(|e| match e {
            TypeEstimate::Class(c) => is_class_sealed(*c),
            // Immediate kinds correspond to seed classes which are
            // sealed by construction (no user code can subclass the
            // fixnum tag bit, etc.).
            TypeEstimate::Integer
            | TypeEstimate::Boolean
            | TypeEstimate::Character
            | TypeEstimate::SingleFloat
            | TypeEstimate::DoubleFloat
            | TypeEstimate::String => true,
            _ => false,
        })
    {
        return Some("every receiver class is sealed".to_string());
    }
    None
}

fn is_class_sealed(class_id: u32) -> bool {
    let p = class_metadata_ptr(ClassId(class_id));
    if p.is_null() {
        return false;
    }
    // SAFETY: pointer is to static-area metadata (process-lived).
    unsafe { (*p).is_sealed() }
}

fn class_subclass_check(sub: u32, sup: u32) -> bool {
    is_subclass(ClassId(sub), ClassId(sup))
}

/// True iff every concrete instance compatible with `estimates` is
/// `<: m.specialisers`. Used to enumerate applicable methods.
fn is_guaranteed_applicable(m: &Method, estimates: &[TypeEstimate]) -> bool {
    if m.specialisers.len() != estimates.len() {
        return false;
    }
    m.specialisers
        .iter()
        .zip(estimates.iter())
        .all(|(spec, est)| estimate_matches_spec(*est, *spec))
}

fn estimate_matches_spec(est: TypeEstimate, spec: ClassId) -> bool {
    match est {
        TypeEstimate::Class(c) => is_subclass(ClassId(c), spec),
        // Immediate-kind estimates: map to the seed class id and check.
        TypeEstimate::Integer => is_subclass(ClassId::INTEGER, spec),
        TypeEstimate::Boolean => is_subclass(ClassId::BOOLEAN, spec),
        TypeEstimate::Character => is_subclass(ClassId::CHARACTER, spec),
        TypeEstimate::SingleFloat => is_subclass(ClassId::SINGLE_FLOAT, spec),
        TypeEstimate::DoubleFloat => is_subclass(ClassId::DOUBLE_FLOAT, spec),
        TypeEstimate::String => is_subclass(ClassId::BYTE_STRING, spec),
        TypeEstimate::Bottom => true,
        // Top, Unit, Singleton — can't guarantee subtype-ness.
        _ => false,
    }
}

/// Mirror Sprint 13's `compare_specificity` from `nod-runtime` (it's
/// private there). For each argument position, pick the specialiser
/// that appears EARLIER in the receiver's CPL (more specific). The
/// receiver's CPL is determined by the type estimate; for `Class(c)`
/// we use `c.cpl`. For immediate kinds we use the seed class.
fn compare_specificity(a: &Method, b: &Method, estimates: &[TypeEstimate]) -> std::cmp::Ordering {
    for (i, est) in estimates.iter().enumerate() {
        let sa = a.specialisers[i];
        let sb = b.specialisers[i];
        if sa == sb {
            continue;
        }
        let receiver = receiver_class_for_specificity(*est);
        let pa = cpl_position(receiver, sa);
        let pb = cpl_position(receiver, sb);
        return pa.cmp(&pb);
    }
    std::cmp::Ordering::Equal
}

fn receiver_class_for_specificity(est: TypeEstimate) -> ClassId {
    match est {
        TypeEstimate::Class(c) => ClassId(c),
        TypeEstimate::Integer => ClassId::INTEGER,
        TypeEstimate::Boolean => ClassId::BOOLEAN,
        TypeEstimate::Character => ClassId::CHARACTER,
        TypeEstimate::SingleFloat => ClassId::SINGLE_FLOAT,
        TypeEstimate::DoubleFloat => ClassId::DOUBLE_FLOAT,
        TypeEstimate::String => ClassId::BYTE_STRING,
        _ => ClassId::OBJECT,
    }
}

fn cpl_position(receiver: ClassId, spec: ClassId) -> usize {
    let p = class_metadata_ptr(receiver);
    if p.is_null() {
        return usize::MAX;
    }
    // SAFETY: static-area metadata.
    let cpl = unsafe { &(*p).cpl };
    cpl.iter().position(|c| *c == spec).unwrap_or(usize::MAX)
}
