use std::fmt;

macro_rules! str_enum {
    ($name:ident { $($v:ident => $s:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($v),+ }
        impl $name {
            #[allow(dead_code)] // tests walk every value
            pub const ALL: &'static [$name] = &[$($name::$v),+];
            pub fn as_str(self) -> &'static str { match self { $($name::$v => $s),+ } }
            pub fn from_str(s: &str) -> Option<$name> { match s { $($s => Some($name::$v),)+ _ => None } }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str(self.as_str()) }
        }
    };
}

str_enum!(Stage { Queue => "queue", Active => "active", Review => "review", Parked => "parked", Done => "done", Dropped => "dropped" });
str_enum!(AgentState { Working => "working", Waiting => "waiting", Blocked => "blocked", Ended => "ended" });
str_enum!(SessionKind { Task => "task", Pen => "pen" });
str_enum!(PrState { Unknown => "unknown", Waiting => "waiting", ChangesRequested => "changes_requested", Approved => "approved", Merged => "merged", Closed => "closed" });

impl PrState {
    /// Higher is worse: ChangesRequested > Closed > Unknown > Waiting > Approved > Merged.
    pub fn badness(self) -> u8 {
        match self {
            PrState::ChangesRequested => 5,
            PrState::Closed => 4,
            PrState::Unknown => 3,
            PrState::Waiting => 2,
            PrState::Approved => 1,
            PrState::Merged => 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub kind: String,
    pub stage: Stage,
    pub position: f64,
    pub dir: Option<String>,
    pub labels: Vec<String>,
    pub explainer: Option<String>,
    pub todoist_id: Option<String>,
    /// The stage last pushed to Todoist.
    pub synced_stage: Option<String>,
    pub created_at: i64,
    pub stage_at: i64,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub task_id: Option<i64>,
    pub kind: SessionKind,
    pub cwd: String,
    pub state: AgentState,
    pub state_at: i64,
    pub zellij_session: Option<String>,
    pub zellij_pane: Option<String>,
    #[allow(dead_code)] // mirrors the column; only SQL reads it
    pub edited: bool,
    pub current: bool,
}

#[derive(Clone, Debug)]
pub struct Pr {
    #[allow(dead_code)] // mirrors the column
    pub task_id: i64,
    pub root: String,
    pub url: Option<String>,
    pub waiver: Option<String>,
    pub state: PrState,
    pub reviewers: String,
}

#[derive(Clone, Debug)]
pub struct Event {
    #[allow(dead_code)] // mirrors the column
    pub task_id: i64,
    pub at: i64,
    /// "stage", "summary", "pr", "comment" or "flag".
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, PartialEq)]
pub enum Refusal {
    CapFull { max: usize },
    Gate(Vec<String>),
    NotAllowed { from: Stage, to: Stage },
    NotMerged,
    NoSuchTask,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Refusal::CapFull { max } => write!(f, "All {max} active slots are in use. Park or finish a stream first."),
            Refusal::Gate(m) => write!(f, "Not ready for review: {}", m.join("; ")),
            Refusal::NotAllowed { from, to } => write!(f, "Can't move from {from} to {to}."),
            Refusal::NotMerged => write!(f, "Not merged yet. Drop it if you want to abandon it."),
            Refusal::NoSuchTask => write!(f, "No such task."),
        }
    }
}
