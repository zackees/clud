# Ubuntu 25.04 (Oracular)
FROM ubuntu:25.04

ARG USERNAME=dev
ARG USER_UID=1000
ARG USER_GID=1000
ARG OVS_VERSION=1.103.1      # adjust to latest OpenVSCode Server release
ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    SHELL=/bin/bash \
    WORKSPACE=/workspace \
    OPENVSCODE_SERVER_ROOT=/opt/openvscode-server \
    PATH=/opt/openvscode-server/bin:$PATH

# -------- Base tools, sudo, locales --------
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates curl wget git gnupg unzip zip \
        bash zsh sudo tzdata nano vim less \
        build-essential \
        locales && \
    echo "en_US.UTF-8 UTF-8" > /etc/locale.gen && \
    locale-gen && update-ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# -------- Create non-root user with passwordless sudo --------
RUN groupadd --gid ${USER_GID} ${USERNAME} && \
    useradd  --uid ${USER_UID} --gid ${USER_GID} -m ${USERNAME} -s /bin/bash && \
    echo "${USERNAME} ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/90-${USERNAME} && \
    chmod 0440 /etc/sudoers.d/90-${USERNAME} && \
    mkdir -p ${WORKSPACE} && chown -R ${USERNAME}:${USERNAME} ${WORKSPACE}

# -------- Install OpenVSCode Server --------
RUN set -eux; \
    arch="$(uname -m)"; \
    case "$arch" in \
      x86_64)  OVS_ARCH="x64" ;; \
      aarch64) OVS_ARCH="arm64" ;; \
      armv7l)  OVS_ARCH="armhf" ;; \
      *) echo "Unsupported arch: $arch" && exit 1 ;; \
    esac; \
    mkdir -p ${OPENVSCODE_SERVER_ROOT}; \
    curl -fsSL -o /tmp/ovscode.tar.gz \
      "https://github.com/gitpod-io/openvscode-server/releases/download/openvscode-server-v${OVS_VERSION}/openvscode-server-v${OVS_VERSION}-linux-${OVS_ARCH}.tar.gz"; \
    tar -xzf /tmp/ovscode.tar.gz -C ${OPENVSCODE_SERVER_ROOT} --strip-components=1; \
    rm /tmp/ovscode.tar.gz

# -------- Expose OpenVSCode Server port --------
EXPOSE 3000

# -------- Switch to non-root user --------
USER ${USERNAME}
WORKDIR ${WORKSPACE}

# -------- Entry point: launch OpenVSCode Server --------
ENV CONNECTION_TOKEN=changeme
CMD [ "bash", "-lc", "\
  ${OPENVSCODE_SERVER_ROOT}/bin/openvscode-server \
    --host 0.0.0.0 \
    --port 3000 \
    --connection-token '${CONNECTION_TOKEN}' \
    --disable-update-check \
    --disable-telemetry \
    --user-data-dir /home/${USERNAME}/.vscode-server \
    --default-folder /workspace \
  "]
