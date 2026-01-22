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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskKind {
    ThreadList,
    ThreadLoad,
    ThreadRename,
    ThreadTitle,
    ThreadPreview,
    ThreadCreate,
    ThreadFork,
    FileDiscovery,
    Bash,
    Handoff,
    LoginExchange,
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

/// Task lifecycle state (stored in AppState, mutated only by reducer).
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
        self.cancel = started.cancel.clone();
        self.meta = started.meta.clone();
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
pub struct Tasks {
    pub thread_list: TaskState,
    pub thread_load: TaskState,
    pub thread_rename: TaskState,
    pub thread_title: TaskState,
    pub thread_preview: TaskState,
    pub thread_create: TaskState,
    pub thread_fork: TaskState,
    pub file_discovery: TaskState,
    pub bash: TaskState,
    pub handoff: TaskState,
    pub login_exchange: TaskState,
}

impl Tasks {
    pub fn state_mut(&mut self, kind: TaskKind) -> &mut TaskState {
        match kind {
            TaskKind::ThreadList => &mut self.thread_list,
            TaskKind::ThreadLoad => &mut self.thread_load,
            TaskKind::ThreadRename => &mut self.thread_rename,
            TaskKind::ThreadTitle => &mut self.thread_title,
            TaskKind::ThreadPreview => &mut self.thread_preview,
            TaskKind::ThreadCreate => &mut self.thread_create,
            TaskKind::ThreadFork => &mut self.thread_fork,
            TaskKind::FileDiscovery => &mut self.file_discovery,
            TaskKind::Bash => &mut self.bash,
            TaskKind::Handoff => &mut self.handoff,
            TaskKind::LoginExchange => &mut self.login_exchange,
        }
    }

    pub fn is_any_running(&self) -> bool {
        self.thread_list.is_running()
            || self.thread_load.is_running()
            || self.thread_rename.is_running()
            || self.thread_title.is_running()
            || self.thread_preview.is_running()
            || self.thread_create.is_running()
            || self.thread_fork.is_running()
            || self.file_discovery.is_running()
            || self.bash.is_running()
            || self.handoff.is_running()
            || self.login_exchange.is_running()
    }
}
