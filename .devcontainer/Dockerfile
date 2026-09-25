# Based on Anthropic's reference Claude Code devcontainer (node base, zsh,
# delta, default-deny firewall), extended with the pinned Rust toolchain and
# the tools `just ci` needs. Versions are pinned; bump deliberately.
FROM node:24-bookworm

ARG TZ
ENV TZ="$TZ"

ARG CLAUDE_CODE_VERSION=latest
ARG RUST_TOOLCHAIN=1.98.1
ARG JUST_VERSION=1.58.0
ARG CARGO_DENY_VERSION=0.20.2
ARG TYPST_VERSION=0.15.1
ARG GIT_DELTA_VERSION=0.19.2
ARG ZSH_IN_DOCKER_VERSION=1.2.0

RUN apt-get update && apt-get install -y --no-install-recommends \
  less git procps sudo fzf zsh man-db unzip gnupg2 gh \
  iptables ipset iproute2 dnsutils aggregate jq nano vim \
  build-essential pkg-config clang lld xz-utils ca-certificates curl wget \
  postgresql-client redis-tools \
  && apt-get clean && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /usr/local/share/npm-global && chown -R node:node /usr/local/share

ARG USERNAME=node
RUN mkdir /commandhistory && touch /commandhistory/.bash_history \
  && chown -R $USERNAME /commandhistory

ENV DEVCONTAINER=true

RUN mkdir -p /workspace /home/node/.claude /home/node/.cargo/registry /home/node/.cargo/git && \
  chown -R node:node /workspace /home/node/.claude /home/node/.cargo

WORKDIR /workspace

# Release binaries: delta, just, cargo-deny, typst (template authoring).
RUN set -eux; \
  ARCH="$(dpkg --print-architecture)"; \
  case "$ARCH" in amd64) TRIPLE=x86_64-unknown-linux-musl ;; arm64) TRIPLE=aarch64-unknown-linux-musl ;; *) echo "unsupported $ARCH"; exit 1 ;; esac; \
  wget -q "https://github.com/dandavison/delta/releases/download/${GIT_DELTA_VERSION}/git-delta_${GIT_DELTA_VERSION}_${ARCH}.deb"; \
  dpkg -i "git-delta_${GIT_DELTA_VERSION}_${ARCH}.deb"; rm "git-delta_${GIT_DELTA_VERSION}_${ARCH}.deb"; \
  wget -qO- "https://github.com/casey/just/releases/download/${JUST_VERSION}/just-${JUST_VERSION}-${TRIPLE}.tar.gz" | tar -xz -C /usr/local/bin just; \
  wget -qO- "https://github.com/EmbarkStudios/cargo-deny/releases/download/${CARGO_DENY_VERSION}/cargo-deny-${CARGO_DENY_VERSION}-${TRIPLE}.tar.gz" \
    | tar -xz --strip-components=1 -C /usr/local/bin "cargo-deny-${CARGO_DENY_VERSION}-${TRIPLE}/cargo-deny"; \
  wget -qO- "https://github.com/typst/typst/releases/download/v${TYPST_VERSION}/typst-${TRIPLE}.tar.xz" \
    | tar -xJ --strip-components=1 -C /usr/local/bin "typst-${TRIPLE}/typst"; \
  just --version; cargo-deny --version; typst --version

USER node

# Rust, pinned to rust-toolchain.toml, installed at build time so a fresh
# container never needs a toolchain download before its first build.
ENV RUSTUP_HOME=/home/node/.rustup CARGO_HOME=/home/node/.cargo
ENV PATH=/home/node/.cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" -c rustfmt -c clippy -c rust-src \
  && rustc --version

ENV NPM_CONFIG_PREFIX=/usr/local/share/npm-global
ENV PATH=$PATH:/usr/local/share/npm-global/bin
ENV SHELL=/bin/zsh
ENV EDITOR=nano
ENV VISUAL=nano

RUN sh -c "$(wget -O- https://github.com/deluan/zsh-in-docker/releases/download/v${ZSH_IN_DOCKER_VERSION}/zsh-in-docker.sh)" -- \
  -p git -p fzf \
  -a "source /usr/share/doc/fzf/examples/key-bindings.zsh" \
  -a "source /usr/share/doc/fzf/examples/completion.zsh" \
  -a "export PROMPT_COMMAND='history -a' && export HISTFILE=/commandhistory/.bash_history" \
  -x

RUN npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}

COPY init-firewall.sh /usr/local/bin/
# GitHub's ranges as of the date inside the file: the firewall's fallback when
# api.github.com/meta is unreachable or rate limited (60 requests/hour/IP).
# Refresh it when you touch the firewall:
#   curl -s https://api.github.com/meta | jq --arg d "$(date -u +%F)" \
#     '{snapshot_date: $d, source: "https://api.github.com/meta (IPv4 entries of web, api and git only)",
#       web: [.web[] | select(contains(":") | not)], api: [.api[] | select(contains(":") | not)],
#       git: [.git[] | select(contains(":") | not)]}' > .devcontainer/github-meta-snapshot.json
# Under /etc, root-owned: /usr/local/share belongs to `node` (npm globals), and
# a snapshot `node` could replace would let it widen the allowlist.
COPY github-meta-snapshot.json /etc/meta-whatsapp-rs-firewall/github-meta-snapshot.json
USER root
# Login shells (bash -l) reset PATH from /etc/profile; keep cargo and the npm
# global bin (claude) reachable there too, not only in ENV-inheriting shells.
RUN printf 'export PATH="/home/node/.cargo/bin:/usr/local/share/npm-global/bin:$PATH"\n' \
  > /etc/profile.d/meta-whatsapp-rs-path.sh
RUN chmod +x /usr/local/bin/init-firewall.sh && \
  echo "node ALL=(root) NOPASSWD: /usr/local/bin/init-firewall.sh" > /etc/sudoers.d/node-firewall && \
  chmod 0440 /etc/sudoers.d/node-firewall
USER node
