//! What Claude Code is told: the prompt for a worker, a reviewer and a planner, built from the task, what people
//! said on it, the checks' output and the PR's diff, each cut to a size the model's context can spare, and the
//! schema its answer has to fit.

use serde_json::{Value, json};
use trailhead_factory::{CheckResult, Feedback, PlanContext, PlanTask, Role, RunContext, ShotFiles};

use crate::PLAN_TURN_ENV;

/// The structured final answer Claude Code must give, per role.
pub fn schema(role: Role) -> Value {
    let outcomes = match role {
        Role::Reviewer => json!(["approve", "request_changes"]),
        _ => json!(["done", "already_done", "needs_input", "failed"]),
    };
    json!({
        "type": "object",
        "properties": {
            "outcome": { "enum": outcomes },
            "summary": {
                "type": "string",
                "description": "For done: the PR description. For a review: the review comment. For already_done: the evidence (commit, file and line). Otherwise: why.",
            },
            "title": {
                "type": "string",
                "description": "For done: a conventional commit title for the change, `type(scope): message`, with type one of feat, fix, perf, refactor, docs, test, chore, ci, build and scope the crate or area (factory, core, server, cli, daemon, docs). The task key is appended for you. E.g. `fix(factory): send the prompt on stdin so big PRs can be reviewed`.",
            },
            "question": { "type": "string", "description": "For needs_input: the one question for a human." },
            "follow_ups": {
                "type": "array",
                "description": "For an approving review: non-blocking suggestions, each filed as its own task. Leave out anything the change should fix first.",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "priority": { "type": "integer", "minimum": 1, "maximum": 4, "description": "1 (most urgent) to 4." },
                    },
                    "required": ["title"],
                },
            },
            "raise_grade": {
                "type": "object",
                "description": "For a review: only if the change is riskier than its grade (1 walk, 2 ordinary code, 3 dependencies, schema or public API, 4 auth, secrets, egress, CI or the factory's policy), the grade it deserves and why. Never to lower one.",
                "properties": {
                    "grade": { "type": "integer", "minimum": 2, "maximum": 4 },
                    "reason": { "type": "string" },
                },
                "required": ["grade", "reason"],
            },
        },
        "required": ["outcome", "summary"],
    })
}

/// How many open follow-ups a reviewer is shown.
pub(crate) const FOLLOW_UPS_CAP: usize = 40;

/// The follow-ups earlier reviews of the task filed, so a reviewer doesn't file a reworded copy of one, or leave one
/// open that the change has since fixed.
pub(crate) fn follow_ups_section(task: &trailhead_factory::TaskSpec) -> String {
    let follow_ups = &task.context.follow_ups;
    if follow_ups.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nFollow-ups that earlier reviews of this task already filed and that are still open. Don't put any of \
         these in `follow_ups` again, even reworded. If the change under review fixes one, say so in your summary \
         by its key:",
    );
    for f in follow_ups.iter().take(FOLLOW_UPS_CAP) {
        out.push_str(&format!("\n- {}: {}", f.key.0, clip(&f.title, 200)));
        let detail = f.description.lines().next().unwrap_or("").trim();
        if !detail.is_empty() {
            out.push_str(&format!(" ({})", clip(detail, 300)));
        }
    }
    out
}

/// The most the human-context section of a prompt may take, in bytes.
pub(crate) const HUMAN_CONTEXT_CAP: usize = 6000;

/// The most the answers may take of the human-context section, so notes keep the rest.
pub(crate) const ANSWERS_CAP: usize = 4000;

/// A planner's conversation is its answered questions, and a turn that starts a new session needs all of it it can get.
pub(crate) const PLANNER_CONTEXT_CAP: usize = 40_000;

pub(crate) const PLANNER_ANSWERS_CAP: usize = 32_000;

/// Where the task's project builds, when it builds in one directory of the repo: the change stays inside it. Empty for
/// a project that builds in the whole repo.
pub(crate) fn path_section(task: &trailhead_factory::TaskSpec, role: Role) -> String {
    let Some(path) = &task.path else { return String::new() };
    let rule = match role {
        Role::Reviewer => {
            "A change to any file outside it needs the task to ask for it; if the task doesn't, request changes."
        }
        Role::Planner => "Plan work inside it unless the ask says otherwise.",
        _ => "Keep the change inside it unless the task says otherwise.",
    };
    format!("\n\nThis task's project builds in `{path}/`. {rule}")
}

/// The answers and the task's decisions and notes, for after the description. Empty when there are none. Past
/// the caps the oldest answers and notes are dropped; each entry is cut short if very long, and the newest answer
/// is always kept.
pub(crate) fn human_section(task: &trailhead_factory::TaskSpec) -> String {
    human_section_within(task, HUMAN_CONTEXT_CAP, ANSWERS_CAP, 1000)
}

/// [`human_section`] within `cap` bytes, `answers_cap` of them for the answers, each question cut at `cut_at` bytes
/// and each answer at twice that.
pub(crate) fn human_section_within(
    task: &trailhead_factory::TaskSpec,
    cap: usize,
    answers_cap: usize,
    cut_at: usize,
) -> String {
    let ctx = &task.context;
    let mut budget = cap;
    let mut out = String::new();
    let mut answers = Vec::new();
    let mut answers_budget = answers_cap;
    for a in ctx.answers.iter().rev() {
        let entry = format!(
            "\n- Q: {}\n  A ({}, {}): {}",
            clip(&a.question, cut_at),
            a.by,
            a.date,
            clip(&a.answer, 2 * cut_at)
        );
        if entry.len() > answers_budget && !answers.is_empty() {
            break;
        }
        answers_budget = answers_budget.saturating_sub(entry.len());
        budget = budget.saturating_sub(entry.len());
        answers.push(entry);
    }
    if !answers.is_empty() {
        out.push_str("\n\nQuestions you asked earlier and the answers you got:");
        answers.iter().rev().for_each(|e| out.push_str(e));
    }
    let mut kept = Vec::new();
    for n in ctx.notes.iter().rev() {
        let entry = format!("\n- {}, {}: {}", n.author, n.date, clip(&n.body, 2000));
        if entry.len() > budget {
            break;
        }
        budget -= entry.len();
        kept.push(entry);
    }
    if !kept.is_empty() {
        out.push_str("\n\nDecisions and notes on this task, oldest first:");
        kept.iter().rev().for_each(|e| out.push_str(e));
    }
    out
}

/// What a reviewer is told about building and the checks the worker already ran. The don't-build line goes in even
/// with no results, as when reconcile starts the review after a restart: this sandbox can't build either way.
pub(crate) fn checks_section(checks: &[CheckResult]) -> String {
    let mut out = String::from(
        "\n\nThis sandbox can't fetch dependencies, so don't build or run tests: review the diff and read code instead.",
    );
    if checks.is_empty() {
        return out;
    }
    out.push_str(" The worker already ran these checks in its own sandbox, where the dependencies are installed:\n");
    for c in checks {
        out.push_str(&format!(
            "- {}: {}\n",
            c.name,
            if c.skipped {
                "skipped (the change touches none of its paths)"
            } else if c.passed {
                "passed"
            } else {
                "FAILED"
            }
        ));
        if !c.passed {
            out.push_str(&format!("  Output (tail):\n```\n{}\n```\n", clip_tail(&c.output, 2000)));
        }
    }
    out
}

/// The last `max` bytes of `s`, on a character boundary, marked when cut.
pub(crate) fn clip_tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("(cut) …{}", &s[start..])
}

/// `s` cut to at most `max` bytes on a character boundary, marked when cut.
pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (cut)", &s[..end])
}

/// The most of the diff a reviewer's prompt embeds, in bytes. The rest it reads from the checkout.
pub const DIFF_CAP: usize = 100 * 1024;

/// The diff cut to `cap` bytes at a file boundary (a `diff --git` line), with a note saying how to read the rest.
pub(crate) fn capped_diff(diff: &str, cap: usize, base: Option<&str>) -> String {
    if diff.len() <= cap {
        return diff.to_string();
    }
    let mut end = cap;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    // Back up to the start of the file whose diff straddles the cap; one huge first file is cut mid-file.
    let at = diff[..end].rfind("\ndiff --git ").map(|i| i + 1).unwrap_or(end);
    let command = match base {
        Some(base) => format!("`git diff origin/{base}...HEAD` in the checkout"),
        None => "`git diff origin/<base>...HEAD` in the checkout, where <base> is the PR's base branch".into(),
    };
    format!(
        "{}\n[The diff is {} bytes and was cut here at {} bytes. Read the rest with {command}.]",
        diff[..at].trim_end(),
        diff.len(),
        at
    )
}

/// What a reviewer is told about the pictures the worker took: the views the change alters, as files to open.
/// Empty when there are none, which also covers a repo that takes no pictures.
pub(crate) fn shots_section(shots: &[ShotFiles]) -> String {
    if shots.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nThe change alters how these views are drawn. The worker's sandbox drew each before the change and \
         after it; open the PNG files and look at them, since a wrong layout, clipped text or a missing control shows \
         there and not in the diff. Say in your summary what you saw. Views not listed draw the same as before.\n",
    );
    let shown = |p: &Option<std::path::PathBuf>| p.as_ref().map(|p| p.display().to_string());
    for shot in shots {
        out.push_str(&match (shown(&shot.before), shown(&shot.after)) {
            (Some(before), Some(after)) => format!("- {}: before `{before}`, after `{after}`\n", shot.name),
            (None, Some(after)) => format!("- {} (new): `{after}`\n", shot.name),
            (Some(before), None) => format!("- {} (no longer drawn): before `{before}`\n", shot.name),
            (None, None) => String::new(),
        });
    }
    out
}

/// What a reviewer is told about the PR's title, which a squash merge makes the commit subject on main.
pub(crate) fn pr_title_section(title: Option<&str>, key: &str) -> String {
    let shown = match title {
        Some(t) => format!("The PR's title is `{t}`."),
        None => "The PR's title couldn't be read, so don't judge it.".into(),
    };
    format!(
        "\n\n{shown} It must be a conventional commit (`type(scope): message`, which the repo's changelog \
         requires) that fits the change: a fix titled feat, or a wrong scope, is a reason to request changes. The \
         task key comes last in square brackets (`... [{key}]`), not parentheses."
    )
}

/// What a planner does, every turn: the factory's rules for a plan.
pub(crate) const PLANNER_RULES: &str = "You are planning, not building. Read the repo first: its AGENTS.md, the code the ask \
touches and, for an app, the guides the repo points to for the UI kit and the shell (their GUIDE.md files). Make no \
commits and change no files in the checkout.\n\n\
If something only the person can answer would change the plan, ask, all your questions in one message, and only \
those whose answers change the plan. Otherwise write the spec and the plan.\n\n\
The spec is markdown: what is wanted and why, what it does and doesn't do, and what done looks like. It replaces the \
task's description, so keep the ask in it.\n\n\
The plan is the tasks that build it. Each is one PR a worker can finish in one run, with acceptance criteria in its \
description, listed in dependency order, and none has tasks of its own. Give each the grade you propose, by how far \
the damage reaches if it is wrong: 1 for docs and tests; 2 for ordinary code in one module or app; 3 for shared \
code, a public API, the shape of stored data, manifests, or a change across modules; 4 for credentials, CI, lockfile \
packages or agent instructions. Say why in one line. A person approves the plan by grading the tasks; nothing \
starts before that. The first task of a new app writes the app's README.md (what it does and why) from the spec, and \
every task that changes what the app does keeps it up, as one of its acceptance criteria.";

/// How a planner ends a turn.
pub(crate) fn turn_section(path: &std::path::Path) -> String {
    format!(
        "\n\nEnd the turn by writing one JSON file to `{path}` (also in ${PLAN_TURN_ENV}), outside the checkout, then \
         stop:\n```json\n{{\"message\": \"what the person reads: your questions, or a summary of the plan\",\n \
         \"spec\": \"optional: the spec, in markdown\",\n \
         \"tasks\": [{{\"id\": \"a\", \"key\": null, \"title\": \"...\", \"description\": \"... with acceptance criteria\", \
         \"blocked_by\": [\"b\", \"TRAIL-3\"], \"grade\": 2, \"why\": \"one line\"}}],\n \
         \"ready\": false}}\n```\n`tasks`, when present, is the whole plan: a task of the plan you leave out is \
         cancelled, and leaving `tasks` out keeps the plan as it is. Give a new task an `id` of your own and no `key`; \
         give a task that exists its `key`. `blocked_by` names ids or keys. Set `ready` when the plan is ready to \
         approve. Without the file the turn fails.",
        path = path.display(),
    )
}

/// The plan as the engine holds it, which a person may have changed by hand since the last turn.
pub(crate) fn plan_section(plan: &[PlanTask]) -> String {
    if plan.is_empty() {
        return "\n\nThe plan has no tasks yet.".to_string();
    }
    let mut out = String::from(
        "\n\nThe plan as it stands in Trailhead. A person may have changed it by hand since your last turn: this, \
         not your memory, is the plan. Only the open, ungraded tasks are yours to change; a graded or started one is \
         a person's.",
    );
    for t in plan {
        let grade = match (&t.grade, &t.proposed) {
            (Some(g), _) => format!("grade {g}"),
            (None, Some(p)) => format!("proposed grade {} ({})", p.grade, clip(&p.why, 200)),
            (None, None) => "ungraded".into(),
        };
        let blocked = if t.blocked_by.is_empty() {
            String::new()
        } else {
            format!(", blocked by {}", t.blocked_by.iter().map(|k| k.0.as_str()).collect::<Vec<_>>().join(", "))
        };
        out.push_str(&format!("\n\n### {}: {}\n{}, {grade}{blocked}", t.key.0, t.title, t.status));
        if !t.description.trim().is_empty() {
            out.push_str(&format!("\n{}", clip(t.description.trim(), 3000)));
        }
    }
    out
}

/// A planner's prompt. A turn that resumes its session is told only what is new: the person's reply and the plan as
/// it stands.
pub fn planner_prompt(ctx: &RunContext, plan: &PlanContext, resumed: bool) -> String {
    let task = &ctx.task;
    let mut p = if resumed {
        let reply = task.context.answers.last().map_or_else(
            || "(no reply; the task was reopened)".to_string(),
            |a| format!("{} ({}, {})", clip(&a.answer, 4000), a.by, a.date),
        );
        format!("The person replied to your last message:\n{reply}")
    } else {
        let spec = if task.description.trim() != plan.planning.ask.trim() && !task.description.trim().is_empty() {
            format!("\n\nThe spec as it stands:\n{}", task.description)
        } else {
            String::new()
        };
        format!(
            "Plan task {key}: {title}.\n\n{PLANNER_RULES}\n\nThe ask:\n{ask}{spec}{path}{human}",
            key = task.key.0,
            title = task.title,
            ask = plan.planning.ask,
            path = path_section(task, ctx.role),
            human = human_section_within(task, PLANNER_CONTEXT_CAP, PLANNER_ANSWERS_CAP, 6000),
        )
    };
    p.push_str(&plan_section(&plan.planning.plan));
    if let Some(failure) = &task.previous_failure {
        p.push_str(&format!("\n\nThe last turn failed:\n{failure}"));
    }
    p.push_str(&turn_section(&plan.files.turn));
    p
}

pub fn prompt(ctx: &RunContext, feedback: Option<&Feedback>) -> String {
    let task = &ctx.task;
    let mut p = match ctx.role {
        Role::Reviewer => format!(
            "You are reviewing a pull request for task {key}: {title}.\n\n\
             Task:\n{desc}{human}{follow_ups}{path}\n\n\
             The change under review:\n```diff\n{diff}\n```{checks}{shots}\n\n\
             Check it against the task and the repo's conventions (AGENTS.md). Read code in the checkout as needed, \
             but don't edit anything. Approve only if it does what the task asks, correctly and with tests where \
             they belong. Your summary is posted on the PR as the review. When you approve, put anything worth doing \
             later that shouldn't hold this change up in `follow_ups`, each with a title and description: the factory \
             files them as tasks, so don't repeat them in the summary. If the change is riskier than its grade \
             says (a dependency, schema or public API change is grade 3; auth, secrets, sandbox egress, CI or the \
             factory's own policy is grade 4), say so in `raise_grade` with the reason; never to lower it.{pr_title}",
            key = task.key.0,
            title = task.title,
            desc = task.description,
            human = human_section(task),
            follow_ups = follow_ups_section(task),
            path = path_section(task, ctx.role),
            diff = ctx.diff.as_deref().map_or("(no diff)".into(), |d| capped_diff(
                d,
                DIFF_CAP,
                ctx.base_branch.as_deref()
            )),
            checks = checks_section(&ctx.checks),
            shots = shots_section(&ctx.shots),
            pr_title = pr_title_section(ctx.title.as_deref(), &task.key.0),
        ),
        _ => format!(
            "Implement task {key}: {title}.\n\n\
             Task:\n{desc}{human}{path}\n\n\
             Work in this checkout and follow the repo's conventions (AGENTS.md). Make the change, add or update \
             tests, and run the repo's formatter, linter and tests. You don't need to commit or push; everything \
             you change in the checkout is published as one commit and a PR. Don't create nested git repositories in \
             the checkout (no `git init` or `git clone` inside it): the parent's `git add --all` warns about an \
             embedded repository, or fails if it has no commit. If you need another repository for reference, clone it into a temporary directory outside \
             the checkout (e.g. under `/tmp`). If only a human can answer something \
             you need, stop and give needs_input with the question. A blocker in your environment (the network, a \
             missing tool, credentials) is needs_input, not failed: another attempt would meet the same blocker, \
             so say what is missing. If the task can't be done for another reason, give failed and say \
             why. If the task's work is already on the base branch, or no longer applies, change nothing and give \
             already_done, with the evidence (the commit, file and line) in `summary`; a person is asked to close \
             the task. When you finish, give `title`: a conventional commit title for the change, like \
             `fix(factory): send the prompt on stdin so big PRs can be reviewed`, with the type (feat, fix, \
             perf, refactor, docs, test, chore, ci or build) that fits what you did and the crate or area as \
             scope; the factory appends the task key. The factory tracks this task in Trailhead for you, so skip \
             AGENTS.md's \"Tracking work\" steps: don't claim, comment on or close tasks. Put follow-ups in your summary.",
            key = task.key.0,
            title = task.title,
            desc = task.description,
            human = human_section(task),
            path = path_section(task, ctx.role),
        ),
    };
    if let Some(failure) = &task.previous_failure {
        p.push_str(&format!("\n\nA previous attempt at this task failed:\n{failure}\n\nFix what broke."));
    }
    if let Some(f) = feedback {
        p.push_str(&format!("\n\nA reviewer asked for changes:\n{}\n\nAddress them.", f.body));
    }
    p
}
