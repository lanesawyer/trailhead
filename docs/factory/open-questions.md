# Open questions

## To decide

- [x] Build the factory into Trailhead: decided 2026-10-26. Rewriting `docs/PLAN.md` vision, principles and phases is TRAIL-87.
- [ ] Daemon ↔ control plane protocol over Iroh: message set and wire format.
- [ ] Which CLI agent gets the first `Harness` adapter (Claude Code planned).
- [ ] Starting budget numbers per profile, per task and per day; the stall window.
- [ ] Observability backend: self-hosted or hosted.
- [ ] When to move off n0 public relays, and what triggers it.
- [ ] Prompt injection: which inputs count as untrusted (issues, PR comments, dependency docs, the web), and whether reviewers flag instructions found in data.
- [ ] Test services and secrets: how repos declare databases or queues next to `[checks]`, and where test credentials live.
- [ ] Previews: Sprite URLs for running web changes during review.
- [ ] Outcome tracking: success rate, escalations, review rounds and spend per profile and task type.
- [ ] Repo onboarding: an agent proposes stack, checks, profiles and policy for a human to confirm.
- [ ] Names for the factory, the daemon and the todo client ([naming.md](naming.md)).
- [ ] First repo for the MVP (Trailhead itself is the candidate).

## Hands-on tests (MVP M0)

- [ ] Iroh in a Sprite under a locked-down egress policy: direct connection or relay only? Compare against no policy; if relay-only, measure latency and try a self-hosted relay in the same Fly region.
- [ ] Claude Code in a Sprite against the Anthropic connector (base URL, placeholder key); does the 120-second gateway timeout cut off long streams?
- [ ] Publish commits and open a PR through the GitHub connector with the Git Data API.
- [ ] Can a checkpoint seed a different Sprite, or only restore its own?
