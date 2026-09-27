FROM rust:1.94-alpine

# python-name-lint: allow-next-line
RUN apk add --no-cache bash build-base clang git lld llvm openssl-dev pkgconf python3 py3-pip \
    && pip install --break-system-packages soldr
