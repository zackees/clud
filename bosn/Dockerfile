FROM rust:1.94-bookworm

# Bosn derives a managed image identity from this Dockerfile. Keep this
# project-specific label so an unrelated registry cannot claim the same tag.
LABEL org.opencontainers.image.title="clud-bosn-validation"

RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 python3-pip git pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/* \
    && pip3 install --break-system-packages uv soldr
