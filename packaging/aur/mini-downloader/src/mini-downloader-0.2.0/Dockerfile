# Mini Downloader build/dev container — Ubuntu 22.04 for a glibc/WebKitGTK floor that keeps
# AppImage output runnable on older distros.
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

# Tauri v2 system deps
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential curl wget file git ca-certificates pkg-config \
      libwebkit2gtk-4.1-dev libssl-dev libgtk-3-dev \
      libayatana-appindicator3-dev librsvg2-dev libxdo-dev \
      patchelf aria2 ffmpeg python3 \
    && rm -rf /var/lib/apt/lists/*

# yt-dlp (latest standalone build, self-updatable)
RUN curl -L https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp \
      -o /usr/local/bin/yt-dlp && chmod a+rx /usr/local/bin/yt-dlp

# Node 22 + pnpm + web-ext
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y nodejs && rm -rf /var/lib/apt/lists/* \
    && npm install -g pnpm web-ext

# Rust stable + Tauri CLI
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain stable \
    && cargo install tauri-cli --version "^2" --locked

WORKDIR /app
CMD ["bash"]
