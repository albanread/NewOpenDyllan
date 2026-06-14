//! C3 linearisation.
//!
//! Sprint 12 builds the SI special case (the merge is trivial) but
//! ships the full algorithm so Sprint 14 only needs to flip the
//! "reject MI" gate. The implementation matches Dylan's
//! `dispatch.dylan` and Python's `mro()` byte-for-byte.
//!
//! Inputs: a class id, a function that returns each class's direct
//! superclasses, and a function that returns a class's existing CPL
//! (if computed already). Outputs: the C3 linearisation as a `Vec<T>`,
//! or an error if the merge can't be made consistent (cyclic /
//! incompatible orderings).

use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub enum C3Error {
    /// The merge step couldn't find a head — the inheritance graph
    /// has incompatible orderings (e.g. two parents that disagree on
    /// the order of their shared ancestors).
    InconsistentMerge {
        class_name: String,
    },
    /// A direct super has no precomputed CPL — caller forgot to
    /// linearise parents first.
    UnresolvedParent {
        class_name: String,
        parent_name: String,
    },
}

impl std::fmt::Display for C3Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            C3Error::InconsistentMerge { class_name } => write!(
                f,
                "C3 merge failed for `{class_name}`: parents impose conflicting orders"
            ),
            C3Error::UnresolvedParent { class_name, parent_name } => write!(
                f,
                "C3 for `{class_name}`: parent `{parent_name}` has no CPL yet"
            ),
        }
    }
}

/// Compute the C3 linearisation for a class given:
///   - `class_name` — for diagnostics.
///   - `parents` — the class's direct superclasses, in declaration order.
///   - `parent_cpls` — for each parent in `parents`, that parent's CPL
///     (must be precomputed).
///
/// The resulting linearisation starts with `class_name` and ends with
/// the root of the inheritance hierarchy. For single inheritance this
/// is trivially `[class, parent.cpl...]`; for MI the merge step kicks
/// in.
pub fn c3_linearise(
    class_name: &str,
    parents: &[String],
    parent_cpls: &[&[String]],
) -> Result<Vec<String>, C3Error> {
    if parents.len() != parent_cpls.len() {
        return Err(C3Error::InconsistentMerge {
            class_name: class_name.to_string(),
        });
    }
    if parents.is_empty() {
        return Ok(vec![class_name.to_string()]);
    }
    // Single inheritance fast path — also exercised by the SI tests.
    if parents.len() == 1 {
        let mut out = Vec::with_capacity(1 + parent_cpls[0].len());
        out.push(class_name.to_string());
        out.extend(parent_cpls[0].iter().cloned());
        return Ok(out);
    }

    // Multi-parent merge. We follow the Python MRO formulation:
    //   merge( L[P1], L[P2], ..., parents )
    // Repeatedly pick a "good head": an element that appears as the
    // head of at least one input list AND in no tail of any other.
    let mut inputs: Vec<VecDeque<String>> = parent_cpls
        .iter()
        .map(|cpl| cpl.iter().cloned().collect())
        .collect();
    inputs.push(parents.iter().cloned().collect());

    let mut result: Vec<String> = vec![class_name.to_string()];

    while inputs.iter().any(|q| !q.is_empty()) {
        // Pick the first head that's "good".
        let mut picked: Option<String> = None;
        for queue in inputs.iter() {
            let Some(candidate) = queue.front() else {
                continue;
            };
            // Good if no other queue has `candidate` in its tail.
            let bad = inputs.iter().any(|other| {
                if other.is_empty() {
                    return false;
                }
                other.iter().skip(1).any(|x| x == candidate)
            });
            if !bad {
                picked = Some(candidate.clone());
                break;
            }
        }
        let Some(head) = picked else {
            return Err(C3Error::InconsistentMerge {
                class_name: class_name.to_string(),
            });
        };
        // Remove `head` from the front of every queue where it appears.
        for queue in inputs.iter_mut() {
            if queue.front() == Some(&head) {
                queue.pop_front();
            }
        }
        result.push(head);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpl_of(name: &str, parents: &[(&str, &[&str])]) -> Vec<String> {
        // Helper: build a CPL from a parent list whose parents are
        // assumed to be linearised already. Used for fixture-style
        // tests.
        let parent_names: Vec<String> =
            parents.iter().map(|(n, _)| n.to_string()).collect();
        let parent_cpls_owned: Vec<Vec<String>> = parents
            .iter()
            .map(|(_, cpl)| cpl.iter().map(|s| s.to_string()).collect())
            .collect();
        let parent_cpl_refs: Vec<&[String]> =
            parent_cpls_owned.iter().map(|v| v.as_slice()).collect();
        c3_linearise(name, &parent_names, &parent_cpl_refs).expect("c3 ok")
    }

    #[test]
    fn si_chain_two_deep() {
        // <object> → <a> → <b>
        let object = cpl_of("<object>", &[]);
        let a = cpl_of("<a>", &[("<object>", &object.iter().map(String::as_str).collect::<Vec<_>>())]);
        let b = cpl_of("<b>", &[("<a>", &a.iter().map(String::as_str).collect::<Vec<_>>())]);
        assert_eq!(b, vec!["<b>", "<a>", "<object>"]);
    }

    #[test]
    fn si_chain_four_deep() {
        let o = cpl_of("<object>", &[]);
        let a = cpl_of("<a>", &[("<object>", &as_strs(&o))]);
        let b = cpl_of("<b>", &[("<a>", &as_strs(&a))]);
        let c = cpl_of("<c>", &[("<b>", &as_strs(&b))]);
        let d = cpl_of("<d>", &[("<c>", &as_strs(&c))]);
        assert_eq!(d, vec!["<d>", "<c>", "<b>", "<a>", "<object>"]);
    }

    #[test]
    fn diamond_mi_resolves_consistent() {
        // <a> -> <b> \
        //              \
        //                <e>
        //              /
        // <a> -> <c> /
        // <e> direct supers: <b>, <c>. Both have <a> as a parent.
        let a = cpl_of("<a>", &[]);
        let b = cpl_of("<b>", &[("<a>", &as_strs(&a))]);
        let c = cpl_of("<c>", &[("<a>", &as_strs(&a))]);
        let e = cpl_of(
            "<e>",
            &[("<b>", &as_strs(&b)), ("<c>", &as_strs(&c))],
        );
        // Python's MRO for the same shape:
        //   class A: pass
        //   class B(A): pass
        //   class C(A): pass
        //   class E(B, C): pass
        //   E.__mro__ = [E, B, C, A, object]
        assert_eq!(e, vec!["<e>", "<b>", "<c>", "<a>"]);
    }

    #[test]
    fn cycle_detection_via_inconsistent_merge() {
        // Force two parents whose CPLs conflict.
        // p1: [<p1>, <x>, <y>]
        // p2: [<p2>, <y>, <x>]
        // <child>(<p1>, <p2>) — Python rejects this with "Cannot create a consistent method resolution order".
        let r = c3_linearise(
            "<child>",
            &["<p1>".to_string(), "<p2>".to_string()],
            &[
                &["<p1>".to_string(), "<x>".to_string(), "<y>".to_string()],
                &["<p2>".to_string(), "<y>".to_string(), "<x>".to_string()],
            ],
        );
        assert!(matches!(r, Err(C3Error::InconsistentMerge { .. })));
    }

    #[test]
    fn empty_class_is_self_only() {
        let r = c3_linearise("<x>", &[], &[]).unwrap();
        assert_eq!(r, vec!["<x>"]);
    }

    #[test]
    fn si_with_two_parents_in_chain() {
        // class A: pass
        // class B(A): pass
        // class C(B): pass
        // class D(C): pass
        let a = cpl_of("<a>", &[]);
        let b = cpl_of("<b>", &[("<a>", &as_strs(&a))]);
        let c = cpl_of("<c>", &[("<b>", &as_strs(&b))]);
        let d = cpl_of("<d>", &[("<c>", &as_strs(&c))]);
        assert_eq!(d, vec!["<d>", "<c>", "<b>", "<a>"]);
    }

    #[test]
    fn mi_with_shared_grandparent() {
        // class X: pass
        // class A(X): pass
        // class B(X): pass
        // class C(A, B): pass
        // Python MRO: C, A, B, X
        let x = cpl_of("<x>", &[]);
        let a = cpl_of("<a>", &[("<x>", &as_strs(&x))]);
        let b = cpl_of("<b>", &[("<x>", &as_strs(&x))]);
        let c = cpl_of("<c>", &[("<a>", &as_strs(&a)), ("<b>", &as_strs(&b))]);
        assert_eq!(c, vec!["<c>", "<a>", "<b>", "<x>"]);
    }

    #[test]
    fn mi_three_parents_no_shared() {
        // class X: pass
        // class Y: pass
        // class Z: pass
        // class W(X, Y, Z): pass
        let x = cpl_of("<x>", &[]);
        let y = cpl_of("<y>", &[]);
        let z = cpl_of("<z>", &[]);
        let w = cpl_of(
            "<w>",
            &[("<x>", &as_strs(&x)), ("<y>", &as_strs(&y)), ("<z>", &as_strs(&z))],
        );
        assert_eq!(w, vec!["<w>", "<x>", "<y>", "<z>"]);
    }

    #[test]
    fn complex_mi_python_e_example() {
        // The Wikipedia C3 example:
        //   class O: pass
        //   class A(O): pass
        //   class B(O): pass
        //   class C(O): pass
        //   class D(O): pass
        //   class E(O): pass
        //   class K1(A, B, C): pass
        //   class K2(D, B, E): pass
        //   class K3(D, A): pass
        //   class Z(K1, K2, K3): pass
        // Python's MRO for Z: [Z, K1, K2, K3, D, A, B, C, E, O].
        let o = cpl_of("<o>", &[]);
        let a = cpl_of("<a>", &[("<o>", &as_strs(&o))]);
        let b = cpl_of("<b>", &[("<o>", &as_strs(&o))]);
        let c = cpl_of("<c>", &[("<o>", &as_strs(&o))]);
        let d = cpl_of("<d>", &[("<o>", &as_strs(&o))]);
        let e = cpl_of("<e>", &[("<o>", &as_strs(&o))]);
        let k1 = cpl_of(
            "<k1>",
            &[("<a>", &as_strs(&a)), ("<b>", &as_strs(&b)), ("<c>", &as_strs(&c))],
        );
        let k2 = cpl_of(
            "<k2>",
            &[("<d>", &as_strs(&d)), ("<b>", &as_strs(&b)), ("<e>", &as_strs(&e))],
        );
        let k3 = cpl_of("<k3>", &[("<d>", &as_strs(&d)), ("<a>", &as_strs(&a))]);
        let z = cpl_of(
            "<z>",
            &[("<k1>", &as_strs(&k1)), ("<k2>", &as_strs(&k2)), ("<k3>", &as_strs(&k3))],
        );
        assert_eq!(
            z,
            vec!["<z>", "<k1>", "<k2>", "<k3>", "<d>", "<a>", "<b>", "<c>", "<e>", "<o>"]
        );
    }

    #[test]
    fn parent_appears_in_own_cpl_at_index_zero() {
        let r = c3_linearise(
            "<child>",
            &["<parent>".to_string()],
            &[&["<parent>".to_string(), "<grand>".to_string()]],
        )
        .unwrap();
        assert_eq!(r, vec!["<child>", "<parent>", "<grand>"]);
    }

    #[test]
    fn mi_two_parents_disjoint() {
        // class A: pass
        // class B: pass
        // class C(A, B): pass
        let a = cpl_of("<a>", &[]);
        let b = cpl_of("<b>", &[]);
        let c = cpl_of("<c>", &[("<a>", &as_strs(&a)), ("<b>", &as_strs(&b))]);
        assert_eq!(c, vec!["<c>", "<a>", "<b>"]);
    }

    fn as_strs(v: &[String]) -> Vec<&str> {
        v.iter().map(|s| s.as_str()).collect()
    }
}
