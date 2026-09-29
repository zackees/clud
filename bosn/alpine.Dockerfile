FROM rust:1.94-alpine@sha256:77237dd363a0b127bb5ef532c2d64c0deb380b738e43a9c4bdac73398d6d0a08

# python-name-lint: allow-next-line
RUN apk add --no-cache bash build-base clang git lld llvm openssl-dev pkgconf python3 py3-pip \
    && pip install --break-system-packages soldr
