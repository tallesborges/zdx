//! Thread tree derivation for hierarchical display.
//!
//! Transforms a flat list of `ThreadSummary` into a depth-first flattened tree
//! structure for rendering in the thread picker.
//!
//! ## Design
//!
//! - **Source of truth**: `Vec<ThreadSummary>` remains the canonical data
//! - **Derived on-demand**: Tree structure is computed when needed, not stored
//! - **Orphan handling**: Threads whose parent is deleted appear at root level

use std::collections::HashMap;

use zdx_core::core::thread_persistence::ThreadSummary;

/// A thread prepared for hierarchical display.
///
/// Contains a reference to the original summary plus derived display properties.
#[derive(Debug, Clone)]
pub struct ThreadDisplayItem<'a> {
    /// Reference to the original thread summary.
    pub summary: &'a ThreadSummary,
    /// Nesting depth (0 = root, 1 = child of root, etc.).
    pub depth: usize,
    /// Whether this thread was created via handoff.
    pub is_handoff: bool,
}

fn visit_owned<'a>(
    threads: &'a [ThreadSummary],
    idx: usize,
    depth: usize,
    children_by_parent: &HashMap<&str, Vec<usize>>,
    visited: &mut std::collections::HashSet<usize>,
    result: &mut Vec<ThreadDisplayItem<'a>>,
) {
    if visited.contains(&idx) {
        return;
    }
    visited.insert(idx);

    let thread = &threads[idx];
    let is_handoff = thread.handoff_from.is_some();

    result.push(ThreadDisplayItem {
        summary: thread,
        depth,
        is_handoff,
    });

    if let Some(children) = children_by_parent.get(thread.id.as_str()) {
        for &child_idx in children {
            visit_owned(
                threads,
                child_idx,
                depth + 1,
                children_by_parent,
                visited,
                result,
            );
        }
    }
}

fn visit_refs<'a>(
    threads: &[&'a ThreadSummary],
    idx: usize,
    depth: usize,
    children_by_parent: &HashMap<&str, Vec<usize>>,
    visited: &mut std::collections::HashSet<usize>,
    result: &mut Vec<ThreadDisplayItem<'a>>,
) {
    if visited.contains(&idx) {
        return;
    }
    visited.insert(idx);

    let thread = threads[idx];
    let is_handoff = thread.handoff_from.is_some();

    result.push(ThreadDisplayItem {
        summary: thread,
        depth,
        is_handoff,
    });

    if let Some(children) = children_by_parent.get(thread.id.as_str()) {
        for &child_idx in children {
            visit_refs(
                threads,
                child_idx,
                depth + 1,
                children_by_parent,
                visited,
                result,
            );
        }
    }
}

/// Transforms a flat list of threads into a depth-first flattened tree.
///
/// Threads are organized by their `handoff_from` relationships:
/// - Threads without `handoff_from` (or with missing parents) appear at depth 0
/// - Child threads appear immediately after their parent, indented by depth
///
/// The input order is preserved for root-level threads (typically sorted by
/// modification time). Children are inserted after their parent in the order
/// they appear in the input.
///
/// # Example
///
/// Given threads:
/// - A (root, newest)
/// - B (handoff from A)
/// - C (root, older)
/// - D (handoff from B)
///
/// Output order: A (depth=0), B (depth=1), D (depth=2), C (depth=0)
///
/// # See Also
///
/// Use [`flatten_refs_as_tree`] when working with a slice of references
/// (e.g., after filtering threads).
pub fn flatten_as_tree(threads: &[ThreadSummary]) -> Vec<ThreadDisplayItem<'_>> {
    if threads.is_empty() {
        return Vec::new();
    }

    // Build parent -> children index
    // Key: parent thread ID, Value: indices into `threads` slice
    let mut children_by_parent: HashMap<&str, Vec<usize>> = HashMap::new();

    // Track which thread IDs exist (for orphan detection)
    let thread_ids: std::collections::HashSet<&str> =
        threads.iter().map(|t| t.id.as_str()).collect();

    // Identify root threads and build children index
    let mut root_indices: Vec<usize> = Vec::new();

    for (idx, thread) in threads.iter().enumerate() {
        match &thread.handoff_from {
            Some(parent_id) if thread_ids.contains(parent_id.as_str()) => {
                // Valid parent exists - add as child
                children_by_parent
                    .entry(parent_id.as_str())
                    .or_default()
                    .push(idx);
            }
            _ => {
                // No parent or orphan (parent deleted) - treat as root
                root_indices.push(idx);
            }
        }
    }

    // Build flattened output via depth-first traversal
    let mut result: Vec<ThreadDisplayItem<'_>> = Vec::with_capacity(threads.len());

    // Track visited to handle cycles (defensive)
    let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();

    // Process roots in their original order
    for root_idx in root_indices {
        visit_owned(
            threads,
            root_idx,
            0,
            &children_by_parent,
            &mut visited,
            &mut result,
        );
    }

    result
}

/// Like [`flatten_as_tree`] but accepts a slice of thread references.
///
/// Use this when working with filtered thread lists (e.g., threads filtered
/// by workspace scope) where you have `&[&ThreadSummary]` instead of
/// `&[ThreadSummary]`.
///
/// The algorithm is identical to `flatten_as_tree` - threads are organized
/// by their `handoff_from` relationships into a depth-first tree.
pub fn flatten_refs_as_tree<'a>(threads: &[&'a ThreadSummary]) -> Vec<ThreadDisplayItem<'a>> {
    if threads.is_empty() {
        return Vec::new();
    }

    // Build parent -> children index
    let mut children_by_parent: HashMap<&str, Vec<usize>> = HashMap::new();
    let thread_ids: std::collections::HashSet<&str> =
        threads.iter().map(|t| t.id.as_str()).collect();

    let mut root_indices: Vec<usize> = Vec::new();

    for (idx, thread) in threads.iter().enumerate() {
        match &thread.handoff_from {
            Some(parent_id) if thread_ids.contains(parent_id.as_str()) => {
                children_by_parent
                    .entry(parent_id.as_str())
                    .or_default()
                    .push(idx);
            }
            _ => {
                root_indices.push(idx);
            }
        }
    }

    // Build flattened output via depth-first traversal
    let mut result: Vec<ThreadDisplayItem<'a>> = Vec::with_capacity(threads.len());
    let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for root_idx in root_indices {
        visit_refs(
            threads,
            root_idx,
            0,
            &children_by_parent,
            &mut visited,
            &mut result,
        );
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_thread(id: &str, handoff_from: Option<&str>) -> ThreadSummary {
        ThreadSummary {
            id: id.to_string(),
            title: Some(format!("Thread {id}")),
            root_path: None,
            modified: None,
            handoff_from: handoff_from.map(std::string::ToString::to_string),
        }
    }

    #[test]
    fn test_empty_threads() {
        let result = flatten_as_tree(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_single_root_thread() {
        let threads = vec![make_thread("A", None)];
        let result = flatten_as_tree(&threads);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].summary.id, "A");
        assert_eq!(result[0].depth, 0);
        assert!(!result[0].is_handoff);
    }

    #[test]
    fn test_multiple_root_threads_preserve_order() {
        let threads = vec![
            make_thread("A", None),
            make_thread("B", None),
            make_thread("C", None),
        ];
        let result = flatten_as_tree(&threads);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].summary.id, "A");
        assert_eq!(result[1].summary.id, "B");
        assert_eq!(result[2].summary.id, "C");

        for item in &result {
            assert_eq!(item.depth, 0);
            assert!(!item.is_handoff);
        }
    }

    #[test]
    fn test_parent_child_relationship() {
        // A is parent, B is child (handoff from A)
        let threads = vec![make_thread("A", None), make_thread("B", Some("A"))];
        let result = flatten_as_tree(&threads);

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].summary.id, "A");
        assert_eq!(result[0].depth, 0);
        assert!(!result[0].is_handoff);

        assert_eq!(result[1].summary.id, "B");
        assert_eq!(result[1].depth, 1);
        assert!(result[1].is_handoff);
    }

    #[test]
    fn test_nested_handoffs() {
        // A -> B -> C (three levels deep)
        let threads = vec![
            make_thread("A", None),
            make_thread("B", Some("A")),
            make_thread("C", Some("B")),
        ];
        let result = flatten_as_tree(&threads);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].summary.id, "A");
        assert_eq!(result[0].depth, 0);
        assert!(!result[0].is_handoff);

        assert_eq!(result[1].summary.id, "B");
        assert_eq!(result[1].depth, 1);
        assert!(result[1].is_handoff);

        assert_eq!(result[2].summary.id, "C");
        assert_eq!(result[2].depth, 2);
        assert!(result[2].is_handoff);
    }

    #[test]
    fn test_multiple_children_of_same_parent() {
        // A has two children: B and C
        let threads = vec![
            make_thread("A", None),
            make_thread("B", Some("A")),
            make_thread("C", Some("A")),
        ];
        let result = flatten_as_tree(&threads);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].summary.id, "A");
        assert_eq!(result[0].depth, 0);

        // Children appear after parent, in input order
        assert_eq!(result[1].summary.id, "B");
        assert_eq!(result[1].depth, 1);

        assert_eq!(result[2].summary.id, "C");
        assert_eq!(result[2].depth, 1);
    }

    #[test]
    fn test_orphan_treated_as_root() {
        // B claims parent "X" but X doesn't exist
        let threads = vec![make_thread("A", None), make_thread("B", Some("X"))];
        let result = flatten_as_tree(&threads);

        assert_eq!(result.len(), 2);
        // Both are roots (B is orphan)
        assert_eq!(result[0].summary.id, "A");
        assert_eq!(result[0].depth, 0);

        assert_eq!(result[1].summary.id, "B");
        assert_eq!(result[1].depth, 0);
        // B is still marked as handoff (it has handoff_from set)
        assert!(result[1].is_handoff);
    }

    #[test]
    fn test_complex_tree() {
        // Tree structure:
        //   A (root, newest)
        //   ├── B (handoff from A)
        //   │   └── D (handoff from B)
        //   └── E (handoff from A)
        //   C (root, older)
        //
        // Input order preserves modification time (A, B, C, D, E)
        let threads = vec![
            make_thread("A", None),
            make_thread("B", Some("A")),
            make_thread("C", None),
            make_thread("D", Some("B")),
            make_thread("E", Some("A")),
        ];
        let result = flatten_as_tree(&threads);

        assert_eq!(result.len(), 5);

        // Depth-first: A, then A's children (B, E), then B's children (D), then C
        assert_eq!(result[0].summary.id, "A");
        assert_eq!(result[0].depth, 0);

        assert_eq!(result[1].summary.id, "B");
        assert_eq!(result[1].depth, 1);

        assert_eq!(result[2].summary.id, "D");
        assert_eq!(result[2].depth, 2);

        assert_eq!(result[3].summary.id, "E");
        assert_eq!(result[3].depth, 1);

        assert_eq!(result[4].summary.id, "C");
        assert_eq!(result[4].depth, 0);
    }

    #[test]
    fn test_cycle_detection() {
        // Malformed data: A -> B -> A (cycle)
        // Neither thread has a valid root, so both are excluded from the tree.
        // This is acceptable behavior for malformed data.
        let threads = vec![
            ThreadSummary {
                id: "A".to_string(),
                title: Some("Thread A".to_string()),
                root_path: None,
                modified: None,
                handoff_from: Some("B".to_string()),
            },
            ThreadSummary {
                id: "B".to_string(),
                title: Some("Thread B".to_string()),
                root_path: None,
                modified: None,
                handoff_from: Some("A".to_string()),
            },
        ];
        let result = flatten_as_tree(&threads);

        // Cycles have no clear root, so threads are not included.
        // This is acceptable for malformed data - users would need to fix
        // the thread metadata manually if this ever occurred.
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_child_appears_before_parent_in_input() {
        // Child B listed before parent A (unusual but valid)
        let threads = vec![make_thread("B", Some("A")), make_thread("A", None)];
        let result = flatten_as_tree(&threads);

        assert_eq!(result.len(), 2);
        // Parent should appear first in output
        assert_eq!(result[0].summary.id, "A");
        assert_eq!(result[0].depth, 0);

        assert_eq!(result[1].summary.id, "B");
        assert_eq!(result[1].depth, 1);
    }
}
