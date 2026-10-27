# Verification and review

Three layers, cheapest first. A per-repo policy file assigns each change a risk tier and says which layers that tier must pass.

1. **Mechanical checks.** The repo's standard checks ([sandbox.md](sandbox.md)), run in the author's sandbox. A failure goes back to the author agent, not to review.
2. **Agent review.** A separate agent in a fresh sandbox sees the task, the diff and the check results, never the author's transcript. Different prompt, and where possible a different model.
3. **Human review.** A Trailhead review request with the diff, the agents' reports and spend so far.

## Default tiers

| Tier | Example changes | Default gates |
| --- | --- | --- |
| T0 | Docs, tests only, formatting | Checks; auto-merge |
| T1 | Ordinary code within existing modules | Checks + agent review; auto-merge |
| T2 | New dependencies, schema or migrations, public API changes | Checks + agent review + human |
| T3 | Auth, secrets handling, infra, CI, the factory itself, policy or profile files | Human required; agent review advisory |

## Per-repo gate policy

Decided: gates per tier are configurable per repo. The table above is a default. A repo can be fully autonomous, with no human gate at any tier; there, the top tier requires a quorum of independent agents.

```toml
[gates.t1]
require = ["checks", "agent_review"]
auto_merge = true

[gates.t3]
require = ["checks", "quorum"]   # or ["checks", "agent_review", "human"]
quorum = { reviewers = 3, agree = 3, distinct_models = true }
```

Decided: quorum reviewers must be independent.

- **Different models or providers** (`distinct_models`).
- **Fresh sandboxes and contexts**, no access to the author's transcript.
- **Blind first pass.** Each reviewer records a verdict before seeing the others; disagreement escalates.

## The ratchet

Decided: agents may tighten the policy under the repo's normal gates, but any change that loosens it always needs a human, even in a fully autonomous repo. Loosening includes dropping a gate, lowering a quorum, raising a budget, or moving work to a cheaper profile. The control plane decides tighten vs loosen by comparing old and new policy, not by trusting the author's description. Trailhead already applies the same rule to `require_review` and project reviewers.

## Rules that keep the gate honest

- The tier is computed by the control plane from the diff (paths, dependency files, size), never self-declared by the author.
- A change touching the policy or profile files is top tier.
- Any reviewer can escalate a tier through Trailhead; nobody can lower one.
- Author and reviewer disagree twice → escalate to a human.
