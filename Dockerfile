# Mini Downloader build/dev container — Ubuntu 22.04 for a glibc/WebKitGTK floor that keeps
# AppImage output runnable on older distros.
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

# Tauri v2 system deps
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends \
      build-essential curl wget file git ca-certificates pkg-config \
      libwebkit2gtk-4.1-dev libssl-dev libgtk-3-dev \
      libayatana-appindicator3-dev librsvg2-dev libxdo-dev \
      patchelf aria2 ffmpeg python3 \
    && rm -rf /var/lib/apt/lists/*

# yt-dlp (latest standalone build, self-updatable)
RUN curl -fsSL https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp \
      -o /usr/local/bin/yt-dlp && chmod a+rx /usr/local/bin/yt-dlp

# Node 22 + project-aligned JavaScript tooling.  Keep these majors in sync
# with CI so the container catches the same lockfile and extension issues.
RUN curl -fsSL https://deb.nodesource.com/setup_22.x -o /tmp/nodesource-setup.sh \
    && bash /tmp/nodesource-setup.sh \
    && rm /tmp/nodesource-setup.sh \
    && apt-get install -y -qq nodejs && rm -rf /var/lib/apt/lists/* \
    && npm install -g pnpm@11 web-ext@8

# Rust stable. The project-local, lockfile-pinned Tauri CLI is installed by
# pnpm during Dev Container creation and Compose builds.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain stable

# Docker creates named volumes as root; the Dev Container uses sudo once to
# hand its dependency volume to the unprivileged developer user.
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends sudo \
    && rm -rf /var/lib/apt/lists/*

# The image remains root-by-default for the existing Compose workflow, while
# the Dev Container attaches as this unprivileged user.  That keeps generated
# workspace files owned by the developer rather than by root on Linux hosts.
# pnpm's store stays in the container home too: a bind-mounted workspace is a
# different filesystem, which would otherwise make pnpm create a host-local
# store with container-specific metadata.
RUN useradd --create-home --shell /bin/bash vscode \
    && install -d -o vscode -g vscode /home/vscode/.config/pnpm \
    && printf 'storeDir: /home/vscode/.pnpm-store\n' > /home/vscode/.config/pnpm/config.yaml \
    && printf 'vscode ALL=(root) NOPASSWD:ALL\n' > /etc/sudoers.d/vscode \
    && chmod 0440 /etc/sudoers.d/vscode \
    && chown -R vscode:vscode "$CARGO_HOME" "$RUSTUP_HOME"

WORKDIR /app
CMD ["bash"]
