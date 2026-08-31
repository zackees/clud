# Container check for the `rm -rf $VAR/` guard.
#
# The image contains the hook binary and a corpus of tool-call payloads. It
# runs the hook against each one and asserts the verdict. It never executes a
# removal: the command text lives inside a JSON payload on stdin, and the hook
# only ever prints a decision about it.
#
# The binary is built outside and copied in, rather than compiled here, so the
# check runs in seconds and tests the same artifact CI already produces. That
# couples the base image to the build host's libc: the default matches the
# ubuntu-24.04 runner this is meant to run on. Building on a host with a
# different libc (or a distro that relocates the ELF interpreter, as NixOS
# does) needs a matching base or a static binary, or the binary will not start
# and every case will report exit 127.
#
# Build context is the repo root:
#   docker build -f ci/docker/rm_protection/Dockerfile \
#     --build-arg BINARY=target/debug/clud-block-bad-cmd -t clud-rm-protection .
ARG BASE=ubuntu:24.04
FROM ${BASE}

# python3 generates the payload corpus; bash runs the assertions. Neither ever
# hands a case to a shell.
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

ARG BINARY
COPY ${BINARY} /usr/local/bin/clud-block-bad-cmd
COPY ci/docker/rm_protection/generate_cases.py /opt/rm-protection/generate_cases.py
COPY ci/docker/rm_protection/verify.sh /opt/rm-protection/verify.sh
RUN chmod +x /usr/local/bin/clud-block-bad-cmd /opt/rm-protection/verify.sh

# Unprivileged, with a writable scratch dir the hook can log into. Nothing of
# value is mounted, so even a defect here has nothing to delete.
RUN useradd --create-home --uid 10001 checker \
    && mkdir -p /work \
    && chown checker:checker /work
USER checker
ENV HOME=/home/checker \
    WORK_DIR=/work \
    CASE_DIR=/tmp/rm-protection-cases

ENTRYPOINT ["/opt/rm-protection/verify.sh"]
