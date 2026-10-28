# The default sandbox image for the Docker Compute: what the stock Sprite image gives an agent (git, a C toolchain,
# rustup and Claude Code), as the unprivileged `trailhead` user. The daemon binary comes in at start, so it must be
# built against this image's glibc or older. `mise run docker-image` builds it as trailhead-sandbox:latest.
FROM ubuntu:26.04

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git build-essential pkg-config \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --shell /bin/bash trailhead

USER trailhead
WORKDIR /home/trailhead
ENV PATH=/home/trailhead/.local/bin:/home/trailhead/.cargo/bin:$PATH

RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --component clippy,rustfmt \
 && curl -fsSL https://claude.ai/install.sh | bash \
 && claude --version
