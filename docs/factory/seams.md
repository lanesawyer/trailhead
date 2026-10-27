# Plugin seams

Decided: the factory core is agnostic to compute, agent harness, model provider, task tracker and code host. Each is a trait with one implementation per vendor. Iroh and the standard checks stay fixed; they're the factory's own machinery, not vendor choices.

| Seam | Runs in | First implementation | Later |
| --- | --- | --- | --- |
| `Compute` | Control plane | Fly.io Sprites | Other microVM hosts, local VMs |
| `Harness` | Daemon, in the sandbox | Claude Code | Codex, an own agent loop |
| `ModelProvider` | Both | Anthropic | OpenAI, OpenRouter |
| `Tracker` | Both | Trailhead (in-process in the server, REST from the daemon) | Jira and others |
| `CodeHost` | Both | GitHub | GitLab, Forgejo |

## Trait sketches

Draft signatures; the testkit fakes are the first test of whether they're right. The code in `crates/trailhead-factory` is now authoritative; M1 changed these:

- **`Daemon`**, the control plane's side of the daemon contract (`run` with an event stream, `cancel`). The control plane never calls `Harness` itself: the daemon hosts it, runs checks, publishes and opens the PR. `trailhead-testkit`'s `LocalDaemon` runs it in-process until the iroh daemon lands in M3.
- **`Tracker`** handles are bound to one identity. `sub_agent(name)` acts as a sandbox's sub-agent (Trailhead's `sub_agent_ctx` already allows it), which is how the control plane parks or requeues a task the sandbox holds without waiting for the "parent acts on sub-agents' tasks" engine change. `identity_for` became `create_sub_agent`/`revoke_sub_agent`; `show`, `release` and `in_flight` (for reconcile) were added. `events()` waits for the in-process tracker in M4.
- **`Compute::set_labels`** is the kill switch: removing the `role` label cuts connector access.
- **`CodeHost::diff`** gives the reviewer the change without the author's transcript.
- **`ModelProvider::spend`** waits for the daily cap, which is out of the MVP.
- **Run metadata** (`RunUpdate`): `attempt`, `sandbox`, `profile`, `pr`, `review_rounds`, `reviewed`, `spend`. Reviewer sandboxes and sub-agents are named per round (`trail-42-a1-r1`), so a review interrupted by a crash can be retried under a fresh identity.

```rust
#[async_trait]
pub trait Compute {
    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxId>;   // name, labels, egress (no base: checkpoints can't seed another sandbox)
    async fn list(&self, labels: &Labels) -> Result<Vec<Sandbox>>;
    async fn checkpoint(&self, id: &SandboxId, note: &str) -> Result<CheckpointId>;
    async fn restore(&self, id: &SandboxId, cp: &CheckpointId) -> Result<()>;
    async fn set_egress(&self, id: &SandboxId, egress: &Egress) -> Result<()>;
    async fn destroy(&self, id: &SandboxId) -> Result<()>;
}

#[async_trait]
pub trait Harness {
    async fn run(&self, ctx: RunContext, events: EventSink) -> Result<RunOutcome>;  // task, workdir, model endpoint, budget
    async fn resume(&self, ctx: RunContext, feedback: Feedback, events: EventSink) -> Result<RunOutcome>;
}

#[async_trait]
pub trait ModelProvider {
    fn resolve(&self, alias: ModelAlias) -> ModelId;                     // opus, sonnet, haiku -> concrete id
    fn endpoint(&self) -> Endpoint;                                      // e.g. the connector gateway URL
    fn price(&self, model: &ModelId, usage: &Usage) -> Cost;
    async fn spend(&self, window: TimeWindow) -> Result<Cost>;           // provider usage API
}

#[async_trait]
pub trait Tracker {
    async fn ready(&self, project: &ProjectKey, limit: usize) -> Result<Vec<Task>>;
    async fn claim(&self, task: &TaskKey) -> Result<Claim>;
    async fn renew(&self, task: &TaskKey) -> Result<()>;
    async fn report(&self, task: &TaskKey, update: RunUpdate) -> Result<()>;   // metadata: spend, PR, attempt
    async fn request_review(&self, task: &TaskKey) -> Result<()>;
    async fn record_verdict(&self, task: &TaskKey, v: Verdict) -> Result<()>;
    async fn ask(&self, task: &TaskKey, question: &str) -> Result<()>;
    async fn finish(&self, task: &TaskKey, handoff: &str) -> Result<()>;
    async fn identity_for(&self, task: &TaskKey) -> Result<Credential>;        // scoped sub-agent
    fn events(&self) -> BoxStream<'_, TrackerEvent>;
}

#[async_trait]
pub trait CodeHost {
    async fn publish(&self, repo: &Repo, branch: &str, commits: &[Commit]) -> Result<()>;
    async fn open_pr(&self, repo: &Repo, pr: NewPr) -> Result<PrRef>;
    async fn comment(&self, pr: &PrRef, body: &str) -> Result<()>;
    async fn merge(&self, pr: &PrRef) -> Result<MergeOutcome>;           // Conflict sends the task back to Running
}
```

## Rules

- **Vendor features map onto neutral concepts.** Sprites connectors become a sandbox's `role` label plus an `Egress` list; another compute host might grant the same role with injected env or a proxy. Checkpoints are required of every `Compute`, since rollback depends on them.
- **Static plugins.** Implementations are crates compiled in behind Cargo features, chosen by name in config. No dynamic loading or hot swapping. Traits use `async_trait` (or enum dispatch) so config can pick one at runtime.
- **Fakes for testing.** An in-memory `Compute`, a scripted `Harness` that applies canned edits, and fake `Tracker` and `CodeHost` run the whole lifecycle in CI with no network or spend. No external emulator such as spritzer.

## Crate layout (draft)

| Crate | Contents |
| --- | --- |
| `trailhead-core` | Task engine: schema, claims, events, undo (exists) |
| `trailhead-server` | REST, MCP, SSE, webhooks (exists); hosts the control plane when the factory feature is on |
| `trailhead` | CLI (exists); gains factory commands |
| `trailhead-factory` | Factory domain types, daemon protocol, the five traits, config schema |
| `trailhead-control` | Orchestrator, reconciler, review router, budget meter; in-process `Tracker` |
| `trailhead-daemon` | In-sandbox daemon: harness host, checks runner, checkpointing; REST `Tracker` |
| `trailhead-compute-sprites` | `Compute` for Fly.io Sprites (own thin API wrapper) |
| `trailhead-harness-claude-code` | `Harness` wrapping the Claude Code CLI |
| `trailhead-provider-anthropic` | `ModelProvider` for Anthropic |
| `trailhead-codehost-github` | `CodeHost` for GitHub's REST API |
| `trailhead-testkit` | Fakes for all five seams |
| `trailhead-ticklist` | Todo client (later; name TBD) |

Factory crates are `publish = false` until they're ready, since the release workflow packages the whole workspace.
