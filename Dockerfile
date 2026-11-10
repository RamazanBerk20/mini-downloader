# Native Linux build/dev container. Ubuntu 22.04 keeps the glibc floor usable
# on older supported distributions without pulling in a web rendering engine.
FROM ubuntu:22.04@sha256:3b06811b2afd352be909dd088a004166d665dc76d38b13eada33522a9d915c6f

ARG RUST_TOOLCHAIN=1.97.1
# Versioned archive URL: `rustup/dist/` always serves the newest rustup, so a
# fixed checksum against it breaks on every upstream release.
ARG RUSTUP_VERSION=1.29.0
ARG RUSTUP_INIT_SHA256=4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10
ARG YTDLP_VERSION=2026.07.04
ARG YTDLP_SHA256=495be29ff4d9d4e9be7eabdfef225221e5d5282e77f2f505abc6dca80349f3fd

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends \
      build-essential curl git ca-certificates pkg-config sudo \
      libfontconfig1-dev libwayland-dev libxkbcommon-dev \
      libx11-dev libx11-xcb-dev libxcb1-dev libxcursor-dev \
      libxrandr-dev libxi-dev \
      aria2 ffmpeg python3 \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL --retry 3 --retry-all-errors --connect-timeout 15 --max-time 600 \
      "https://github.com/yt-dlp/yt-dlp/releases/download/${YTDLP_VERSION}/yt-dlp" \
      -o /usr/local/bin/yt-dlp \
    && echo "${YTDLP_SHA256}  /usr/local/bin/yt-dlp" | sha256sum -c - \
    && chmod 0755 /usr/local/bin/yt-dlp

RUN curl --proto '=https' --tlsv1.2 -fsSL --retry 3 --retry-all-errors \
      --connect-timeout 15 --max-time 600 \
      "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init" \
      -o /tmp/rustup-init \
    && echo "${RUSTUP_INIT_SHA256}  /tmp/rustup-init" | sha256sum -c - \
    && chmod 0755 /tmp/rustup-init \
    && /tmp/rustup-init -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" \
    && rm -f /tmp/rustup-init

RUN useradd --create-home --shell /bin/bash vscode \
    && printf 'vscode ALL=(root) NOPASSWD:ALL\n' > /etc/sudoers.d/vscode \
    && chmod 0440 /etc/sudoers.d/vscode \
    && mkdir -p /app/target \
    && chown -R vscode:vscode "$CARGO_HOME" "$RUSTUP_HOME" /app

WORKDIR /app
USER vscode
CMD ["bash"]
