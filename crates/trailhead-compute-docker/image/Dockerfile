# The default sandbox image for the Docker Compute: what the stock Sprite image gives an agent (git, a C toolchain,
# rustup, Node with corepack and Claude Code), as the unprivileged `trailhead` user. The daemon binary comes in at
# start, so it must be built against this image's glibc or older. `mise run docker-image` builds it as
# trailhead-sandbox:latest.
FROM ubuntu:26.04

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git build-essential pkg-config \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --shell /bin/bash trailhead

# Node, pinned, from the official tarball (checked against the release's SHA-256 list), with corepack enabled so pnpm
# and yarn come from a repo's `packageManager` field. Node 24 still bundles corepack; later lines don't.
ARG NODE_VERSION=24.11.0
RUN apt-get update \
 && apt-get install -y --no-install-recommends xz-utils \
 && arch="$(dpkg --print-architecture | sed 's/amd64/x64/')" \
 && file="node-v${NODE_VERSION}-linux-${arch}.tar.xz" \
 && cd /tmp \
 && curl -fsSLO "https://nodejs.org/dist/v${NODE_VERSION}/${file}" \
 && curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt" | grep " ${file}\$" | sha256sum -c - \
 && tar -xJf "${file}" -C /usr/local --strip-components=1 --no-same-owner \
    --exclude='*/CHANGELOG.md' --exclude='*/README.md' --exclude='*/share/doc' --exclude='*/share/man' \
 && rm -f "${file}" \
 && corepack enable \
 && apt-get purge -y --auto-remove xz-utils \
 && rm -rf /var/lib/apt/lists/*

USER trailhead
WORKDIR /home/trailhead
ENV PATH=/home/trailhead/.local/bin:/home/trailhead/.cargo/bin:$PATH

# pnpm is fetched now, so `pnpm --version` works with no registry; a repo's `packageManager` field overrides it.
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --component clippy,rustfmt \
 && curl -fsSL https://claude.ai/install.sh | bash \
 && corepack prepare pnpm@10.20.0 --activate \
 && claude --version \
 && node --version \
 && pnpm --version
