# Disposable GNU-rm process checks for #1461. Hazardous operands only reach a
# recording next-PATH stub; the real-mount test uses a read-only disposable bind.
ARG BASE=ubuntu:24.04
FROM ${BASE}

RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-venv ca-certificates git && apt-get clean # python-name-lint: allow

RUN python3 -m venv /opt/venv && /opt/venv/bin/pip install --no-cache-dir pytest==9.0.3 running-process==4.10.1 # python-name-lint: allow

ARG CLUD_BINARY=target/debug/clud
ARG HOOK_BINARY=target/debug/clud-cmd-scan
ARG SHIM_BINARY=target/debug/clud-shim
COPY ${CLUD_BINARY} /usr/local/bin/clud
COPY ${HOOK_BINARY} /usr/local/bin/clud-cmd-scan
COPY ${SHIM_BINARY} /usr/local/bin/clud-shim
COPY ${SHIM_BINARY} /build/target/debug/clud-shim
COPY tests/__init__.py tests/process.py tests/test_rm_shim.py tests/test_mount_floor.py /workspace/tests/
COPY ci/docker/rm_protection/stub_cases.json /workspace/ci/docker/rm_protection/stub_cases.json

RUN useradd --create-home --uid 10001 checker
USER checker
WORKDIR /workspace
ENV PATH=/opt/venv/bin:/usr/local/bin:/usr/bin:/bin \
    CLUD_TEST_BINARY=/usr/local/bin/clud \
    PYTHONDONTWRITEBYTECODE=1 \
    HOME=/home/checker
CMD ["python", "-m", "pytest", "-p", "no:cacheprovider", "-q", "tests/test_rm_shim.py::test_old_stub_corpus_maps_to_new_floor_and_hook"]
