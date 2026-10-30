# Observability

> **Design note.** It records decisions and intent; where it says something is built or not, check the code if it matters.

Proposal: instrument everything with `tracing` and export through OpenTelemetry, so the backend is a later, swappable choice. Trailhead already logs with `tracing`, carrying request and batch IDs.

- **OpenTelemetry** is a vendor-neutral CNCF standard for traces, metrics and logs. Instrument once, send over its wire protocol (OTLP), and point it at any backend that speaks OTLP. In Rust: `tracing` + `tracing-opentelemetry` + `opentelemetry-otlp`. Conventions for LLM calls (model, tokens, latency) exist but are still evolving.
- **What it buys here:** one trace per task, spanning control plane → Iroh message → daemon → agent step → connector call. Carry trace context in every Iroh message header so spans join across machines, and set the Trailhead task key as a span attribute.

| Option | Kind | Notes |
| --- | --- | --- |
| Grafana stack (Tempo, Loki, Prometheus) | Self-host or Grafana Cloud | Most common OSS route; several pieces to run |
| Jaeger | Self-host, traces only | Simple; no logs or metrics |
| SigNoz | Self-host or cloud | One OTel-native app for traces, logs, metrics |
| Honeycomb, Axiom | Hosted | Strong for high-cardinality queries like per-task traces |
| Langfuse, Arize Phoenix | LLM-focused | Prompt and transcript views; can sit alongside a general backend |
| Plain `tracing` JSON logs | No standard | Fastest start; no cross-service traces |

Starting point: `tracing` logs only for the MVP; OTLP export and a backend after. The backend choice is open ([open-questions.md](open-questions.md)).

Human interface: logs for now, then the Bevy app in [ui.md](ui.md).
