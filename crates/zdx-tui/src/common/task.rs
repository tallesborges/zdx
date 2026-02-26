use enum_map::{Enum, EnumMap};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub u64);

#[derive(Debug, Default)]
pub struct TaskSeq {
    next: u64,
}

impl TaskSeq {
    pub fn next_id(&mut self) -> TaskId {
        let id = TaskId(self.next);
        self.next = self.next.wrapping_add(1);
        id
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskKind {
    ThreadList,
    ThreadLoad,
    ThreadRename,
    ThreadTitle,
    ThreadPreview,
    ThreadCreate,
    ThreadFork,
    ThreadWorktree,
    FileDiscovery,
    SkillsFetch,
    SkillInstall,
    Bash,
    Handoff,
    LoginExchange,
    LoginCallback,
    ImageDecode,
}

#[derive(Debug, Clone, Default)]
pub enum TaskMeta {
    #[default]
    None,
    Bash {
        id: String,
        command: String,
    },
    Handoff {
        goal: String,
    },
}

#[derive(Debug, Clone)]
pub struct TaskStarted {
    pub id: TaskId,
    pub cancel: Option<CancellationToken>,
    pub meta: TaskMeta,
}

#[derive(Debug)]
pub struct TaskCompleted<E> {
    pub id: TaskId,
    pub result: E,
}

/// Task lifecycle state (stored in `AppState`, mutated only by reducer).
#[derive(Debug, Default, Clone)]
pub struct TaskState {
    pub active: Option<TaskId>,
    pub cancel: Option<CancellationToken>,
    pub meta: TaskMeta,
}

impl TaskState {
    pub fn is_running(&self) -> bool {
        self.active.is_some()
    }

    pub fn on_started(&mut self, started: &TaskStarted) {
        self.active = Some(started.id);
        self.cancel.clone_from(&started.cancel);
        self.meta.clone_from(&started.meta);
    }

    pub fn finish_if_active(&mut self, id: TaskId) -> bool {
        let ok = self.active == Some(id);
        if ok {
            self.active = None;
            self.cancel = None;
            self.meta = TaskMeta::None;
        }
        ok
    }

    pub fn clear(&mut self) {
        self.active = None;
        self.cancel = None;
        self.meta = TaskMeta::None;
    }
}

#[derive(Debug, Default, Clone)]
pub struct Tasks(pub EnumMap<TaskKind, TaskState>);

impl Tasks {
    pub fn state_mut(&mut self, kind: TaskKind) -> &mut TaskState {
        &mut self.0[kind]
    }

    pub fn state(&self, kind: TaskKind) -> &TaskState {
        &self.0[kind]
    }

    pub fn is_any_running(&self) -> bool {
        self.0.iter().any(|(_, state)| state.is_running())
    }
}
