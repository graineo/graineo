# Build environment of the experimental Linux build (scripts/bundle-linux.sh). Ubuntu 22.04 (glibc 2.35) so that
# the binary runs on current distributions.
FROM ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends build-essential pkg-config cmake clang libclang-dev curl ca-certificates git \
 libfontconfig1-dev libfreetype-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
 libgl1-mesa-dev libvulkan-dev libssl-dev libdbus-1-dev libgtk-3-dev libasound2-dev libudev-dev \
 xvfb mesa-vulkan-drivers libvulkan1 vulkan-tools xauth zip file && rm -rf /var/lib/apt/lists/*
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
ENV PATH=/root/.cargo/bin:$PATH
