# Copyright 2026 RealBrakedownPerf Authors
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# Artifact evaluation container for Real Brakedown Performance (RBP-LCPC).
# Builds no Rust binaries into the image: -C target-cpu=native must compile
# on the evaluator's host CPU at run time. All external dependencies and Cargo
# crates are prefetched into /rbd/deps and /root/.cargo so runs work offline.
#
# Build from a full git clone (not a linked worktree where .git is a file):
#   docker build -t rbd-artifact .
# Run (pin physical cores with --cpuset-cpus; avoid --cpus CPU quota limits):
#   docker run --rm --network none --cpuset-cpus 0-7 -v "$PWD/out:/rbd/out" \
#       rbd-artifact reproduce/run.sh --mode quick

FROM ubuntu:24.04@sha256:9c7f200c07e3d4a03e7249d2c16bf66a51120fb876aa2242cf31451412543289

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    g++ \
    cmake \
    git \
    python3 \
    python3-matplotlib \
    util-linux \
    time \
    xz-utils \
    curl \
    ca-certificates \
    libboost-dev \
    latexmk \
    texlive-latex-base \
    texlive-latex-recommended \
    texlive-latex-extra \
    texlive-fonts-recommended \
    texlive-science \
    && rm -rf /var/lib/apt/lists/*

# Install rustup without a default toolchain; rust-toolchain.toml selects 1.99.1.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain none --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

COPY . /rbd
WORKDIR /rbd

# Bake the pinned Rust toolchain from rust-toolchain.toml (1.99.1) without compiling,
# and set it as default so out-of-tree cargo invocations (e.g. cd /) find it.
RUN cargo --version && rustc --version \
    && rustup default "$(rustc --version | awk '{print $2}')"

# Prefetch pinned third-party baselines and all Cargo crates (workspace + baselines + step/* tags).
RUN sh reproduce/fetch_deps.sh && touch deps/.offline

ENV RBD_OFFLINE=1 \
    CARGO_NET_OFFLINE=true
