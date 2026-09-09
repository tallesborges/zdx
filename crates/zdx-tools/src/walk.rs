//! Shared filesystem traversal policy for the `glob` and `grep` tools.
//!
//! Both tools walk the tree with `ignore::WalkBuilder`. Keeping the traversal
//! rule, the wall-clock budget, and the parallel walk here stops the two from
//! drifting apart, and guarantees a single tool call can never walk unbounded.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use globset::Glob;
use ignore::types::Types;
use ignore::{DirEntry, WalkBuilder, WalkState};

/// Wall-clock ceiling for the traversal performed by one tool call.
///
/// Searches rooted at `$HOME`, `/Users`, or a package cache can otherwise walk
/// for minutes. Callers return whatever they collected and mark it partial.
///
/// Soft: a blocking `readdir`/`stat` (iCloud, network mounts) cannot be
/// interrupted, so a call can overshoot. Measured at `$HOME`: usually within
/// 100ms of the deadline, occasionally ~5s over.
pub(crate) const WALK_BUDGET: Duration = Duration::from_secs(5);

/// Deadline shared by every walker thread of a single tool call.
///
/// Cheap to clone. The "partial" flag is shared by the call and any phase
/// derived from it, so a caller can always tell "cut short by the clock" apart
/// from "nothing matched".
#[derive(Debug, Clone)]
pub(crate) struct WalkBudget {
    deadline: Instant,
    partial: Arc<AtomicBool>,
}

impl WalkBudget {
    pub(crate) fn new(budget: Duration) -> Self {
        Self {
            deadline: Instant::now() + budget,
            partial: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn start() -> Self {
        Self::new(WALK_BUDGET)
    }

    /// A sub-budget for one phase of the call, expiring after `portion` but
    /// never after the call's own deadline.
    ///
    /// Lets a caller that has work to do *after* walking (grep searches what it
    /// found) stop the walk before the whole budget is gone, while still
    /// reporting one partial result for the call.
    pub(crate) fn phase(&self, portion: Duration) -> Self {
        Self {
            deadline: (Instant::now() + portion).min(self.deadline),
            partial: Arc::clone(&self.partial),
        }
    }

    /// True once this budget's deadline has passed. Marks the call partial.
    pub(crate) fn is_expired(&self) -> bool {
        if Instant::now() < self.deadline {
            return false;
        }
        self.partial.store(true, Ordering::Relaxed);
        true
    }

    /// True when some phase of this call hit its deadline, i.e. results are partial.
    pub(crate) fn is_partial(&self) -> bool {
        self.partial.load(Ordering::Relaxed)
    }
}

/// Traversal policy for one tool call: what to skip, and when to stop.
///
/// Hidden files are always traversed: dotted paths are ordinary content here
/// (`.github/workflows`, `.zdx/skills`, `.cargo/config.toml`), and both `rg`
/// and `fd` are run with `--hidden` by the agents we compared against. Only
/// `.git` is pruned, the way ripgrep prunes it under `--hidden`.
#[derive(Debug, Clone)]
pub(crate) struct WalkPolicy {
    /// Whether `.ignore`, `.gitignore`, global, and exclude rules prune the walk.
    pub(crate) respect_gitignore: bool,
    /// Whether `.git` is pruned. Off when the caller's pattern names it, so an
    /// explicit request still reaches the repository's own metadata.
    pub(crate) skip_git: bool,
    /// Optional ripgrep-style type filter.
    pub(crate) types: Option<Types>,
    /// Deadline shared by every pass of this call.
    pub(crate) budget: WalkBudget,
}

impl WalkPolicy {
    /// Policy for a search whose file filter is `pattern` (a glob), if any.
    pub(crate) fn for_pattern(pattern: Option<&str>) -> Self {
        Self {
            respect_gitignore: true,
            skip_git: !pattern.is_some_and(pattern_names_git),
            types: None,
            budget: WalkBudget::start(),
        }
    }

    pub(crate) fn with_types(mut self, types: Option<Types>) -> Self {
        self.types = types;
        self
    }

    pub(crate) fn with_include_ignored(mut self, include_ignored: bool) -> Self {
        if include_ignored {
            self.respect_gitignore = false;
        }
        self
    }

    /// Same policy and the same deadline, with ignore-file pruning disabled.
    pub(crate) fn without_gitignore(&self) -> Self {
        Self {
            respect_gitignore: false,
            ..self.clone()
        }
    }

    /// Same policy, with walking limited to `portion` of the call's budget.
    pub(crate) fn with_phase(&self, portion: Duration) -> Self {
        Self {
            budget: self.budget.phase(portion),
            ..self.clone()
        }
    }
}

fn pattern_names_git(pattern: &str) -> bool {
    pattern.split(['/', '\n']).any(|component| {
        component.contains(".git")
            && Glob::new(component).is_ok_and(|glob| glob.compile_matcher().is_match(".git"))
    })
}

/// Depth covered by the sequential first pass.
///
/// The parallel walker interleaves the root's directory read with descent into
/// subtrees, so on a huge tree a file sitting at the root can go unvisited for
/// minutes — measured at `$HOME`: `.zshenv` (depth 1) was reached after 59s and
/// 3.2M entries. A deadline on that order alone would answer "not found" for
/// files that are right there, so the top levels are covered first. Sequential
/// because at this depth the walk is a handful of `readdir` calls (3-9ms in a
/// repo, 213ms at `$HOME`) and thread startup would dominate.
const SHALLOW_DEPTH: usize = 2;

/// Walks `search_path` under `policy`, stopping as soon as the shared budget
/// expires.
///
/// Entries down to [`SHALLOW_DEPTH`] are visited first, in order; everything
/// below is visited in parallel. `visit` is called at most once per entry, from
/// multiple threads in the second pass; returning [`WalkState::Quit`] ends the
/// walk (used for result caps). Unreadable entries are skipped.
pub(crate) fn walk<F>(search_path: &Path, policy: &WalkPolicy, visit: F)
where
    F: Fn(&DirEntry) -> WalkState + Send + Sync,
{
    let mut has_deeper = false;

    for entry in builder(search_path, policy)
        .max_depth(Some(SHALLOW_DEPTH))
        .build()
    {
        if policy.budget.is_expired() {
            return;
        }
        let Ok(entry) = entry else { continue };
        if entry.depth() == SHALLOW_DEPTH && entry.file_type().is_some_and(|ft| ft.is_dir()) {
            has_deeper = true;
        }
        if visit(&entry) == WalkState::Quit {
            return;
        }
    }

    // Nothing below the shallow pass: skip the parallel walk and its startup cost.
    if !has_deeper || policy.budget.is_expired() {
        return;
    }

    let budget = &policy.budget;
    let visit = &visit;
    builder(search_path, policy).build_parallel().run(|| {
        Box::new(move |result| {
            if budget.is_expired() {
                return WalkState::Quit;
            }
            let Ok(entry) = result else {
                return WalkState::Continue;
            };
            // Already covered, exactly, by the shallow pass.
            if entry.depth() <= SHALLOW_DEPTH {
                return WalkState::Continue;
            }
            visit(&entry)
        })
    });
}

/// Builds a walker configured for `policy`, without a depth limit.
fn builder(search_path: &Path, policy: &WalkPolicy) -> WalkBuilder {
    let mut builder = WalkBuilder::new(search_path);
    builder
        .git_ignore(policy.respect_gitignore)
        .git_global(policy.respect_gitignore)
        .git_exclude(policy.respect_gitignore)
        .ignore(policy.respect_gitignore)
        .hidden(false);
    if policy.skip_git {
        builder.filter_entry(|entry| entry.file_name() != ".git");
    }
    if let Some(types) = policy.types.clone() {
        builder.types(types);
    }
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_is_pruned_unless_the_pattern_names_it() {
        assert!(WalkPolicy::for_pattern(None).skip_git);
        assert!(WalkPolicy::for_pattern(Some("**/*.rs")).skip_git);
        assert!(WalkPolicy::for_pattern(Some("**/.github/**")).skip_git);
        assert!(WalkPolicy::for_pattern(Some(".gitignore")).skip_git);
        assert!(WalkPolicy::for_pattern(Some("config\n**/.github/**")).skip_git);
        assert!(!WalkPolicy::for_pattern(Some(".git/config")).skip_git);
        assert!(!WalkPolicy::for_pattern(Some("**/.git/**")).skip_git);
        assert!(!WalkPolicy::for_pattern(Some("**/{.git,.github}/**")).skip_git);
    }

    #[test]
    fn budget_marks_the_call_partial_once_expired() {
        let budget = WalkBudget::new(Duration::ZERO);
        assert!(!budget.is_partial(), "not observed yet");
        assert!(budget.is_expired());
        assert!(budget.is_partial());
    }

    #[test]
    fn budget_with_time_left_is_not_expired() {
        let budget = WalkBudget::new(Duration::from_secs(60));
        assert!(!budget.is_expired());
        assert!(!budget.is_partial());
    }

    #[test]
    fn expired_phase_reports_partial_without_ending_the_call() {
        let call = WalkBudget::new(Duration::from_secs(60));
        let walking = call.phase(Duration::ZERO);

        assert!(walking.is_expired());
        assert!(call.is_partial(), "the call's result is partial");
        assert!(
            !call.is_expired(),
            "work after the walk still has the rest of the budget"
        );
    }

    #[test]
    fn phase_never_outlives_the_call() {
        let call = WalkBudget::new(Duration::ZERO);
        let walking = call.phase(Duration::from_secs(60));
        assert!(walking.is_expired());
    }

    #[test]
    fn without_gitignore_shares_the_deadline() {
        let policy = WalkPolicy::for_pattern(Some(".zshrc"));
        let retry = policy.without_gitignore();

        assert!(policy.respect_gitignore);
        assert!(!retry.respect_gitignore);
        assert!(retry.skip_git, "the rest of the policy carries over");

        assert!(!retry.budget.is_partial());
        policy.budget.partial.store(true, Ordering::Relaxed);
        assert!(retry.budget.is_partial(), "both passes report one result");
    }

    #[test]
    fn walk_visits_every_entry_exactly_once_across_both_passes() {
        // Nested past SHALLOW_DEPTH so the parallel pass runs too.
        let temp = tempfile::TempDir::new().unwrap();
        let deep = temp.path().join("a/b/c/d");
        std::fs::create_dir_all(&deep).unwrap();
        for dir in [
            temp.path(),
            &temp.path().join("a"),
            &temp.path().join("a/b"),
            &temp.path().join("a/b/c"),
            &deep,
        ] {
            std::fs::write(dir.join("file.txt"), "x").unwrap();
        }

        let seen = std::sync::Mutex::new(Vec::new());
        walk(temp.path(), &WalkPolicy::for_pattern(None), |entry| {
            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                seen.lock().unwrap().push(entry.path().to_path_buf());
            }
            WalkState::Continue
        });

        let mut seen = seen.into_inner().unwrap();
        seen.sort();
        let deduped = {
            let mut d = seen.clone();
            d.dedup();
            d
        };
        assert_eq!(seen, deduped, "no entry is visited twice");
        assert_eq!(seen.len(), 5, "shallow and deep files are all visited");
    }
}
