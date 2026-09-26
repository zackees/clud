FROM rust:1.95.0-bookworm

# Bosn derives a managed image identity from this Dockerfile. Keep this
# project-specific label so an unrelated registry cannot claim the same tag.
LABEL org.opencontainers.image.title="clud-bosn-validation"

RUN apt-get update \
    # Debian package names. python-name-lint: allow-next-line
    && apt-get install -y --no-install-recommends clang lld python3 python3-pip python-is-python3 git pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/* \
    && pip3 install --break-system-packages uv soldr
