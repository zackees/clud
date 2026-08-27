FROM rust:1.94-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 python3-pip git pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/* \
    && pip3 install --break-system-packages uv soldr
