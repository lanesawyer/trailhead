//! What the running control plane knows that neither Trailhead nor the compute labels do: the shapes of the factory
//! status snapshot and event stream (`docs/factory/ui.md#factory-status`). The control plane keeps them in memory and
//! serves them; `trailhead-factory status` and `logs --follow` and, later, the UI read them. None of it is stored.

use serde::{Deserialize, Serialize};

use crate::{Grade, TaskKey, Usd};

/// What the control plane itself is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ControlState {
    /// Starting runs and watching them.
    Up,
    /// Putting things right after a boot or a failed run.
    Reconciling,
    /// Following a run the last control plane started, which the daemon still has.
    Reattaching { task: TaskKey },
    /// Stopped. Only seen by a client that kept its connection across the stop.
    Stopped,
}

/// A pooled sandbox's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    Idle,
    Busy,
    /// Held for a parked task, suspended until it is answered.
    Paused,
    /// Out of the pool, waiting to be destroyed.
    Broken,
}

impl SandboxState {
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxState::Idle => "idle",
            SandboxState::Busy => "busy",
            SandboxState::Paused => "paused",
            SandboxState::Broken => "broken",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxStatus {
    pub name: String,
    pub target: Option<String>,
    pub state: SandboxState,
    /// The task it is held for, when it is.
    pub task: Option<TaskKey>,
    /// `worker` or `reviewer`.
    pub role: Option<String>,
    /// Whether it carries a warm checkpoint.
    pub warm: bool,
    /// True for a sandbox one run made for itself (a reviewer's), not a pooled one.
    pub own: bool,
}

/// Where a run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum RunPhase {
    /// The sandbox is being restored and its checkout prepared. `cold` when no warm checkpoint was there to start from.
    Preparing {
        cold: bool,
    },
    Agent,
    Checks,
    Publishing,
    Reviewing,
}

impl RunPhase {
    pub fn label(self) -> &'static str {
        match self {
            RunPhase::Preparing { cold: true } => "preparing (cold)",
            RunPhase::Preparing { cold: false } => "preparing",
            RunPhase::Agent => "agent",
            RunPhase::Checks => "checks",
            RunPhase::Publishing => "publishing",
            RunPhase::Reviewing => "reviewing",
        }
    }
}

/// A run in flight, with what it has done so far.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStatus {
    pub task: TaskKey,
    #[serde(flatten)]
    pub phase: RunPhase,
    pub sandbox: Option<String>,
    pub steps: u32,
    pub tool_calls: u32,
    /// The task's spend so far, earlier runs included.
    pub spend: Usd,
    pub last_tool: Option<String>,
    /// Unix seconds.
    pub started: i64,
}

/// A paid target's spend against its daily cap (`docs/factory/targets.md#spend`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSpend {
    pub target: String,
    /// What it has cost today, estimated.
    pub spent: Usd,
    /// The daily cap.
    pub cap: Usd,
    /// Whether it has reached the cap, so takes no new runs.
    pub capped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactorySnapshot {
    pub control: ControlState,
    /// The last error the control plane logged as one, and when (unix seconds).
    pub last_error: Option<(i64, String)>,
    pub sandboxes: Vec<SandboxStatus>,
    pub runs: Vec<RunStatus>,
    /// Why the merger is holding the factory's merges, when it is: a person's PR with auto-merge on goes first
    /// (TRAIL-804).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holding: Option<String>,
    /// Today's spend on each target that has a daily cap. Not on the event stream: it is as fresh as the last
    /// placement pass.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<TargetSpend>,
    /// The grades whose gate (`[factory.gates.N]`, or the default table) wants a person, so a UI can say where Treeline
    /// is without assuming the table. Not on the stream: it only changes with the config.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub human_grades: Vec<Grade>,
    /// The repo's queue tasks (`queue` in `[factory]`), whose subtasks the factory works. Lookout's Factory filter
    /// uses them to tell the factory's tasks from the rest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<TaskKey>,
    /// The sequence number of the last event the snapshot includes, so a client can join the stream after it.
    pub seq: u64,
}

/// One thing that changed. Tool calls and steps are here and nowhere else: they never become Trailhead events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum FactoryEvent {
    Control {
        control: ControlState,
    },
    Error {
        message: String,
    },
    SandboxChanged {
        sandbox: SandboxStatus,
    },
    SandboxGone {
        name: String,
    },
    RunPhase {
        task: TaskKey,
        #[serde(flatten)]
        phase: RunPhase,
    },
    RunEnded {
        task: TaskKey,
    },
    Step {
        task: TaskKey,
        step: u32,
        spend: Usd,
    },
    ToolCall {
        task: TaskKey,
        name: String,
        detail: Option<String>,
    },
    /// The merger started or stopped holding merges.
    Holding {
        holding: Option<String>,
    },
}

impl FactoryEvent {
    /// The task an event is about, for following one task's stream.
    pub fn task(&self) -> Option<&TaskKey> {
        match self {
            FactoryEvent::RunPhase { task, .. }
            | FactoryEvent::RunEnded { task }
            | FactoryEvent::Step { task, .. }
            | FactoryEvent::ToolCall { task, .. } => Some(task),
            FactoryEvent::SandboxChanged { sandbox } => sandbox.task.as_ref(),
            FactoryEvent::Control { control: ControlState::Reattaching { task } } => Some(task),
            _ => None,
        }
    }

    /// A line for a person.
    pub fn line(&self) -> String {
        match self {
            FactoryEvent::Control { control } => format!("control plane: {}", control_label(control)),
            FactoryEvent::Error { message } => format!("error: {message}"),
            FactoryEvent::Holding { holding: Some(why) } => format!("holding merges: {why}"),
            FactoryEvent::Holding { holding: None } => "no longer holding merges".to_string(),
            FactoryEvent::SandboxChanged { sandbox } => format!("sandbox {} {}", sandbox.name, sandbox.state.as_str()),
            FactoryEvent::SandboxGone { name } => format!("sandbox {name} gone"),
            FactoryEvent::RunPhase { task, phase } => format!("{} {}", task.0, phase.label()),
            FactoryEvent::RunEnded { task } => format!("{} run ended", task.0),
            FactoryEvent::Step { task, step, spend } => format!("{} step {step} (spend {spend})", task.0),
            FactoryEvent::ToolCall { task, name, detail } => {
                format!("{} tool {name} {}", task.0, detail.as_deref().unwrap_or("")).trim_end().to_string()
            }
        }
    }
}

pub fn control_label(c: &ControlState) -> String {
    match c {
        ControlState::Up => "up".into(),
        ControlState::Reconciling => "reconciling".into(),
        ControlState::Reattaching { task } => format!("reattaching to {}", task.0),
        ControlState::Stopped => "stopped".into(),
    }
}

/// An event with its place in the stream. The numbers start from 1 on each boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sequenced {
    pub seq: u64,
    #[serde(flatten)]
    pub event: FactoryEvent,
}
