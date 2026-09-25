# Inert JSON command corpus, guaranteed dry-run argv cases, and a separate
# disposable-file CI + Docker execution gate test. No hazardous source is run.
# Build with the hook and packaged shim artifacts from the same build.
ARG BASE=ubuntu:24.04
FROM ${BASE}

# Python generates the payload corpus; bash runs the assertions. Neither ever
# hands a case to a shell.
RUN apt-get update \
    # Debian package names. python-name-lint: allow-next-line
    && apt-get install -y --no-install-recommends python3 python3-venv proot ca-certificates

RUN python3 -m venv /opt/venv && /opt/venv/bin/pip install --no-cache-dir running-process==4.10.1 # Debian image has no `python`; python-name-lint: allow

ARG SHIM_BINARY=target/debug/clud-shim
COPY ${SHIM_BINARY} /usr/local/bin/clud-shim
RUN mkdir /opt/rm-shims && cp /usr/local/bin/clud-shim /opt/rm-shims/rm && chmod 755 /opt/rm-shims/rm
COPY ci/docker/rm_protection/stub_cases.json /opt/rm-protection/stub_cases.json
COPY ci/docker/rm_protection/verify_stub.py /opt/rm-protection/verify_stub.py
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
ENV PATH=/opt/rm-shims:/opt/venv/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME=/home/checker \
    WORK_DIR=/work \
    CASE_DIR=/tmp/rm-protection-cases

ENTRYPOINT ["/opt/rm-protection/verify.sh"]
