FROM docker:27-cli AS docker-cli

FROM debian:bookworm-slim

# Bosn derives a managed image identity from this Dockerfile. Keep this
# project-specific label so an unrelated registry cannot claim the same tag.
LABEL org.opencontainers.image.title="clud-bosn-act"

# act runs .github/workflows/ci.yml's Linux jobs by driving the host Docker
# engine through the mounted socket; this image only carries the clients.
# The tarball is checked against a pinned digest (the same pin zccache's act
# stack uses); bump both together.
ARG ACT_VERSION=0.2.88
ARG ACT_SHA256=1eb9996682dfcc053ac8f3f90f2ec50376f0cdfc229712d82da03d673c63a2b3
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL "https://github.com/nektos/act/releases/download/v${ACT_VERSION}/act_Linux_x86_64.tar.gz" \
       -o /tmp/act.tar.gz \
    && echo "${ACT_SHA256}  /tmp/act.tar.gz" | sha256sum --check - \
    && tar -xzf /tmp/act.tar.gz -C /usr/local/bin act \
    && rm -f /tmp/act.tar.gz \
    && act --version
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
