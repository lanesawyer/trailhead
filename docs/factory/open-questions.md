# Open questions

> **Undecided.** These are open questions and tests, not how the factory works. Checked boxes are settled; the answers are in the linked pages.

## To decide

- [x] Build the factory into Trailhead: decided 2026-10-26. Rewriting `docs/PLAN.md` vision, principles and phases is TRAIL-87.
- [x] Daemon ↔ control plane protocol over Iroh: built in M3 ([agents.md](agents.md#the-contract)).
- [x] Which CLI agent gets the first `Harness` adapter: Claude Code (`trailhead-harness-claude-code`).
- [ ] Starting budget numbers per profile, per task and per day; the stall window.
- [ ] Observability backend: self-hosted or hosted.
- [ ] When to move off n0 public relays, and what triggers it.
- [ ] Prompt injection: which inputs count as untrusted (issues, PR comments, dependency docs, the web), and whether reviewers flag instructions found in data.
- [ ] Test services and secrets: how repos declare databases or queues next to `[checks]`, and where test credentials live.
- [ ] Previews: Sprite URLs for running web changes during review.
- [ ] Outcome tracking: success rate, escalations, review rounds and spend per profile and task type.
- [ ] Repo onboarding: an agent proposes stack, checks, profiles and policy for a human to confirm.
- [ ] Names for the factory, the daemon and the todo client ([naming.md](naming.md)).
- [ ] The factory status surface the UI and `factory status`/`logs` share: its shape, and how clients reach it ([ui.md](ui.md#factory-status)).
- [x] First repo for the MVP: Trailhead itself.

## Hands-on tests (MVP M0)

- [x] Iroh in a Sprite under a locked-down egress policy: relay only by default, about 5 ms over direct on n0's relay. Allowing and resolving a DNS name for the peer's IP restores direct ([sandbox.md](sandbox.md#iroh-in-a-locked-down-sandbox)). A self-hosted relay isn't needed yet.
- [ ] Claude Code in a Sprite against the Anthropic connector (base URL, placeholder key); does the 120-second gateway timeout cut off long streams? Assumed to work for now; Lane will test it (TRAIL-97). (The MVP ran on a Claude OAuth token instead, see [sandbox.md](sandbox.md); unsure whether this is still wanted.)
- [x] Publish commits and open a PR through the GitHub connector with the Git Data API: works (lanesawyer/trailhead#3, since closed). A public repo clones in 0.4 s with `github.com` on the bootstrap allowlist ([sandbox.md](sandbox.md#credentials-through-sprites-connectors)).
- [ ] Private repos (deferred, TRAIL-96; the MVP uses public repos): `git clone` and `git push` through a `custom_api` connector to `https://github.com` with a fine-grained token.
- [x] Can a checkpoint seed a different Sprite? No, a checkpoint only restores its own Sprite. Replaced by bootstrapping each Sprite from the stock image ([sandbox.md](sandbox.md#bootstrap-and-checkpoints)).
- [x] Can Sprite labels be set at create time and changed afterwards through the API? Yes: create takes `labels`, and `PUT` replaces them. Values like `repo=lanesawyer/trailhead` are accepted (live test, 2026-10-26). Still to check: the gateway enforcing a label change (TRAIL-95).
