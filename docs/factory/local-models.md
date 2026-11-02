# Local models

> **Design note.** It records decisions and intent; where it says something is built or not, check the code if it matters.

Where planning on 2026-10-27 landed. Not scheduled soon; this records the direction so the model seams don't close it off.

Tasks: TRAIL-125 (generalize `ModelProvider`), TRAIL-119 (Docker `Compute`), TRAIL-126 (local provider).

## Where the model runs

Leaning: the model runs on the machine that hosts the sandboxes, never inside a sandbox. Sprites have no GPU, and loading a model into every container wastes memory. So the model server (Ollama, llama.cpp, vLLM) is a shared service on the compute host.

This is the same shape as model access today. A sandbox never calls Anthropic directly; it calls a gateway outside itself that injects the credential and forwards. Local models only change where that gateway sits relative to the model:

| Compute | Gateway | Models |
| --- | --- | --- |
| Sprites | Fly's connector gateway | Hosted APIs (Anthropic, OpenRouter, ...) |
| Docker (TRAIL-119) | The gateway container on the Docker engine ([sandbox.md](sandbox.md#the-gateway-credentials-and-egress-for-docker-sandboxes)) | Hosted APIs, or a model server on the same host |

On Docker the model server is one more upstream behind the gateway (TRAIL-175), so sandboxes keep a single egress destination and model traffic never leaves the host. When the control plane, the Docker sandboxes and the model server share one machine, nothing "phones home" anywhere.

Leaning: Sprites stay on hosted models. Sprites block private IPs, so a Sprite can't reach a model server on a private network. Tunnelling model calls through the daemon's iroh connection was considered and dropped: it makes the control plane relay model traffic. If a self-hosted model is ever wanted from a Sprite, the route is a `custom_api` connector to a public endpoint.

## Seam changes

`ModelProvider` and `ModelAlias` are shaped around Anthropic. TRAIL-125 generalizes them, which any second provider needs anyway:

- **Open aliases.** Profiles name models the factory file defines (`local-coder`), mapped per provider, instead of the closed `opus | sonnet | haiku` enum.
- **Endpoint per run.** The endpoint depends on the compute target, so it's resolved per run rather than fixed per provider. It carries a protocol (Anthropic or OpenAI) so a harness can refuse one it can't speak.
- **Budgets beyond USD.** A local model's price is about zero, so its profiles are capped by minutes, steps and tokens. See [budgets.md](budgets.md).
- **Capacity.** A GPU serves a few runs at once. That's a hard concurrency limit, not a rate limit, so the scheduler treats it as slots, like the Sprite pool.

## Harness

Claude Code speaks the Anthropic Messages API, which some local servers serve (or LiteLLM translates to); verify before relying on it. Its prompts and tool use are tuned for Claude and its system prompt is large, so watch for silent truncation (Ollama's default context is small; set 32–64k or more). Models trained on OpenAI-style tool calling likely do better in a harness built for them (Codex in OSS mode, opencode, an own loop). Expect local models to bring a second `Harness` eventually.

## Where local models fit

Through profiles and escalation ([agents.md](agents.md)):

- **Utility** (log summaries, PR text, failure classification): the first and clearest use.
- **First-pass reviewer**: also gives "different model for review" a model from another family.
- **Worker on T0/T1**, with `escalate_to` a Claude profile.
- **Planner and T2/T3 review** stay on Claude.

## Hardware

The maintainer's machine today: 30 GB RAM, 12 cores, an AMD RX 7700 XT/7800 XT (12–16 GB VRAM; llama.cpp via Vulkan or ROCm). Enough to prove the plumbing and run utility work with a small model. Not enough for worker agents: prefill on 20–60k-token agent contexts is slow, and the model competes for RAM with containers running builds. Worker-grade local models wait for a bigger server.
