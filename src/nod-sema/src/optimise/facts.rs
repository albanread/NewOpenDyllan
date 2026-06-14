//! Per-library sealing facts table — populated by `nod-sema::lower` as
//! it walks `Item::DefineClass { modifiers }`, `Item::DefineGeneric
//! { modifiers }`, and `Item::DefineOther { keyword: "domain", ... }`.
//! Read by the dispatch resolver.
//!
//! Sprint 15 single-library scope: each compilation unit has its own
//! `SealingFacts`. Sprint 29's cross-library library-merge will merge
//! per-library tables when sealing facts need to span libraries.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use nod_runtime::{
    ClassId, ClassMetadata, class_metadata_ptr, find_class_id_by_name, for_each_class,
};

/// Sealing facts collected during lowering. Keyed by name (not by
/// `ClassId` / generic-pointer) so the lowering pass can populate
/// this BEFORE the runtime metadata is built.
#[derive(Clone, Debug, Default)]
pub struct SealingFacts {
    /// `define sealed domain g (<A>, <B>);` declarations — keyed by
    /// generic name. Each value is a list of specialiser tuples (one
    /// per `define sealed domain` declaration on that generic). The
    /// dispatch resolver consults this when the generic itself isn't
    /// sealed but a particular specialiser shape is closed.
    pub domains: HashMap<String, Vec<Vec<ClassId>>>,
    /// Generic names that bear the `sealed` modifier on their
    /// `define generic`.
    pub sealed_generics: HashSet<String>,
    /// Class names that bear the `sealed` modifier on their
    /// `define class`.
    pub sealed_classes: HashSet<String>,
}

impl SealingFacts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a `define sealed class <C>`. Adding twice is a no-op.
    pub fn record_sealed_class(&mut self, name: &str) {
        self.sealed_classes.insert(name.to_string());
    }

    /// Record a `define sealed generic g`. Adding twice is a no-op.
    pub fn record_sealed_generic(&mut self, name: &str) {
        self.sealed_generics.insert(name.to_string());
    }

    /// Record a `define sealed domain g (<A>, <B>);`. Multiple domains
    /// on the same generic are kept distinct.
    pub fn record_sealed_domain(&mut self, generic: &str, specialisers: Vec<ClassId>) {
        let entry = self.domains.entry(generic.to_string()).or_default();
        if !entry.iter().any(|e| e == &specialisers) {
            entry.push(specialisers);
        }
    }

    pub fn is_sealed_class(&self, name: &str) -> bool {
        self.sealed_classes.contains(name)
    }

    pub fn is_sealed_generic(&self, name: &str) -> bool {
        self.sealed_generics.contains(name)
    }

    pub fn sealed_domains_for(&self, generic: &str) -> &[Vec<ClassId>] {
        self.domains
            .get(generic)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Render the per-process sealing facts in the format described in
/// spec 15 §7.1. Walks every registered class + generic and prints
/// the sealed ones; included sealed-domain declarations are listed
/// under their generic.
///
/// Sprint 15 single-library scope: the "library" label is fixed because
/// we don't model multi-library compilation yet. Sprint 29 will switch
/// to per-library iteration.
pub fn dump_sealed(library: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Sealing facts in `{library}`:");
    out.push('\n');

    // Sealed classes (with direct subclasses).
    let mut sealed_classes: Vec<&'static ClassMetadata> = Vec::new();
    for_each_class(|md| {
        if md.is_sealed() {
            sealed_classes.push(md);
        }
    });
    sealed_classes.sort_by_key(|m| m.id.0);
    let _ = writeln!(out, "  Sealed classes ({}):", sealed_classes.len());
    for md in &sealed_classes {
        let subs = md.direct_subclasses_snapshot();
        let names: Vec<String> = subs
            .iter()
            .map(|c| {
                let p = class_metadata_ptr(*c);
                if p.is_null() {
                    format!("<unknown:{}>", c.0)
                } else {
                    // SAFETY: static-area metadata, lives for process.
                    unsafe { (*p).name.clone() }
                }
            })
            .collect();
        let _ = writeln!(
            out,
            "    {:<20} direct_subclasses=[{}]",
            md.name,
            names.join(", ")
        );
    }
    out.push('\n');

    // Sealed generics + their methods + their domains.
    let mut sealed_generics: Vec<&'static nod_runtime::GenericFunction> = Vec::new();
    let mut sealed_domain_generics: Vec<&'static nod_runtime::GenericFunction> = Vec::new();
    nod_runtime::for_each_generic(|g| {
        if g.is_sealed() {
            sealed_generics.push(g);
        } else if !g.sealed_domains_snapshot().is_empty() {
            sealed_domain_generics.push(g);
        }
    });
    sealed_generics.sort_by(|a, b| a.name.cmp(&b.name));
    sealed_domain_generics.sort_by(|a, b| a.name.cmp(&b.name));

    let _ = writeln!(out, "  Sealed generics ({}):", sealed_generics.len());
    for g in &sealed_generics {
        let methods = g.methods.read().expect("methods rwlock poisoned");
        let param_count = methods.first().map(|m| m.specialisers.len()).unwrap_or(0);
        let _ = writeln!(
            out,
            "    {:<20} ({param_count} specialiser{}, {} method{})",
            g.name,
            if param_count == 1 { "" } else { "s" },
            methods.len(),
            if methods.len() == 1 { "" } else { "s" }
        );
    }
    out.push('\n');

    let total_domain_count: usize = sealed_domain_generics
        .iter()
        .map(|g| g.sealed_domains_snapshot().len())
        .sum();
    let _ = writeln!(out, "  Sealed domains ({total_domain_count}):");
    for g in &sealed_domain_generics {
        for dom in g.sealed_domains_snapshot() {
            let names: Vec<String> = dom
                .iter()
                .map(|c| {
                    let p = class_metadata_ptr(*c);
                    if p.is_null() {
                        format!("<unknown:{}>", c.0)
                    } else {
                        // SAFETY: static-area metadata.
                        unsafe { (*p).name.clone() }
                    }
                })
                .collect();
            let _ = writeln!(out, "    {}({})", g.name, names.join(", "));
        }
    }
    out
}

/// Materialise the sealing facts table from the parsed module and the
/// pre-registered class metadata. Records `Modifier::Sealed` flags on
/// `define class` / `define generic`, and any `define sealed domain`
/// declarations whose body fragments are parseable.
///
/// Called by `lower_module_full` AFTER class registration so the
/// per-class metadata exists for the sealed-class marking step.
pub(crate) fn collect_sealing_facts(
    items: &[nod_reader::Item],
    user_classes: &std::collections::HashMap<String, ClassId>,
) -> SealingFacts {
    let mut facts = SealingFacts::new();
    for item in items {
        match item {
            nod_reader::Item::DefineClass { modifiers, name, .. }
                if modifiers.contains(&nod_reader::Modifier::Sealed) =>
            {
                facts.record_sealed_class(name);
                // Flip the sealed bit on the runtime metadata. We
                // do it here (not in `register_class`) because Sprint
                // 12's `register_class` doesn't know about sealing —
                // the lowering layer owns the modifier interpretation.
                if let Some(cid) = user_classes
                    .get(name)
                    .copied()
                    .or_else(|| find_class_id_by_name(name))
                {
                    let p = class_metadata_ptr(cid);
                    if !p.is_null() {
                        // SAFETY: static-area metadata; `mark_sealed`
                        // uses an atomic-bool store.
                        unsafe { (*p).mark_sealed() };
                    }
                }
            }
            nod_reader::Item::DefineGeneric { modifiers, name, .. }
                if modifiers.contains(&nod_reader::Modifier::Sealed) =>
            {
                facts.record_sealed_generic(name);
                // Mark the runtime generic's sealed flag. The
                // generic may not exist yet — Sprint 13 registers
                // generics lazily on first method. Create it here
                // so the sealed bit is observable before any
                // method registration.
                let g = nod_runtime::get_or_create_generic(name);
                g.mark_sealed();
            }
            nod_reader::Item::DefineOther {
                keyword,
                name,
                body_fragments,
                ..
            } => {
                if keyword != "domain" {
                    continue;
                }
                // Sprint 15: Sprint 04's `parse_define_other` currently
                // consumes the head paren list `(<A>, <B>)` silently
                // before capturing the body, so `body_fragments` is
                // empty for typical `define sealed domain g (<A>);`
                // forms. We leave the lowering hook alive and document
                // the gap; in-process tests and the REPL install
                // sealed domains programmatically via
                // `SealingFacts::record_sealed_domain` (and the runtime
                // shim `GenericFunction::register_sealed_domain`).
                //
                // Sprint 04 follow-up: preserve the head paren list as
                // a fragment so the resolver can decode it without a
                // SourceMap dependency. Tracked in DEFERRED.md.
                let Some(generic_name) = name.clone() else {
                    continue;
                };
                let _ = body_fragments;
                let _ = generic_name;
            }
            _ => {}
        }
    }
    let _ = user_classes;
    facts
}

/// Install the per-library sealing facts onto the runtime registry so
/// the dispatch resolver and `dump_sealed` can read them. Idempotent.
pub(crate) fn install_sealing_facts(facts: &SealingFacts) {
    for (generic_name, domains) in &facts.domains {
        let g = nod_runtime::get_or_create_generic(generic_name);
        for d in domains {
            g.register_sealed_domain(d.clone());
        }
    }
    for name in &facts.sealed_generics {
        let g = nod_runtime::get_or_create_generic(name);
        g.mark_sealed();
    }
    // Sealed classes were marked at collection time; nothing else needed.
}

