FROM docker:27-cli AS docker-cli

FROM debian:bookworm-slim

# Bosn derives a managed image identity from this Dockerfile. Keep this
# project-specific label so an unrelated registry cannot claim the same tag.
LABEL org.opencontainers.image.title="clud-bosn-act"

# act runs .github/workflows/ci.yml's Linux jobs by driving the host Docker
# engine through the mounted socket; this image only carries the clients.
ARG ACT_VERSION=0.2.88
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL "https://github.com/nektos/act/releases/download/v${ACT_VERSION}/act_Linux_x86_64.tar.gz" \
       | tar -xz -C /usr/local/bin act \
    && act --version
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
