# `rm -rf $VAR/` container check

Asserts, inside a container, that the cmd-scan hook refuses every shape of
`rm -rf $VAR/` that would expand to `rm -rf /` — and that it still allows
ordinary work.

Background: [#1064](https://github.com/zackees/clud/issues/1064) (the incident
and the fail-closed fix), [#963](https://github.com/zackees/clud/issues/963)
(the interpreter), [#1068](https://github.com/zackees/clud/pull/1068) (the
stress corpus this mirrors).

## The invariant: nothing here ever runs a removal

Every case is a JSON tool-call payload. It reaches the hook as

```sh
printf '%s' "$payload" | clud-block-bad-cmd
```

— on stdin, as data. The hook parses it and prints a verdict. The command text
inside is never expanded, evaluated, or passed to a shell, so there is nothing
to execute even though thousands of the payloads literally read `rm -rf /`.

**The container is defense in depth for that property, not permission to relax
it.** A guard is verified by asking the guard, not by measuring the blast
radius. If you ever find yourself wanting to execute a case to see what really
happens, stop: that is the one thing this check must never do.

## Files

| File | Role |
| --- | --- |
| `generate_cases.py` | Builds the corpus by crossing removal spellings × hazardous operands × shell structures, plus indirect removals, unprovable assignments, and a benign set. Pure data generation — it spawns nothing. |
| `verify.sh` | Feeds each payload to the hook and checks the verdict. Refusal is exit 2 *and* a `deny` decision; both are asserted, since exit 2 without a decision leaves the harness no reason to show. |
| `Dockerfile` | Minimal image: the hook binary, the two scripts, an unprivileged user. |

## Running it

In CI: the **RM protection (Docker)** workflow, run manually via
`workflow_dispatch` (`gh workflow run rm-protection-docker.yml`).

Locally, from the repo root:

```sh
soldr cargo build -p clud --bin clud-block-bad-cmd
docker build -f ci/docker/rm_protection/Dockerfile \
  --build-arg BINARY=target/debug/clud-block-bad-cmd \
  -t clud-rm-protection:local .
docker run --rm --network none --read-only --cap-drop ALL \
  --tmpfs /tmp --tmpfs /work --tmpfs /home/checker \
  clud-rm-protection:local
```

The binary is copied in rather than rebuilt, so **the base image must match the
build host's libc**. `BASE` defaults to `ubuntu:24.04` to match the CI runner;
override it (`--build-arg BASE=...`) when building elsewhere. A mismatch is
loud rather than silent — the binary fails to start and every case reports exit
127, which the script reports as a failure.

On a distro that relocates the ELF interpreter (NixOS), the host-built binary
will not start under any stock base image. Mounting the store read-only
(`-v /nix/store:/nix/store:ro`) is enough to run it locally.

## Checking that the check can fail

A test that cannot fail proves nothing. Both directions are easy to confirm by
substituting a stub for the hook:

```sh
# Always allow -> hazardous half must fail (0/N refused, exit 1)
printf '#!/bin/sh\nexit 0\n' > /tmp/stub && chmod +x /tmp/stub
docker run --rm --tmpfs /tmp --tmpfs /work --tmpfs /home/checker \
  -v /tmp/stub:/usr/local/bin/clud-block-bad-cmd:ro clud-rm-protection:local
```

Stubbing the opposite way (always exit 2 with a `deny` decision) fails the
benign half instead — which is the point of having one. A guard that refuses
everything is an outage, not a guard.

## Extending the corpus

Add to an axis in `generate_cases.py` rather than appending one-off lines: a new
removal spelling, hazardous operand, or shell structure multiplies through the
cross product. Keep the exhaustive version in sync with
`block_bad_cmd_rm_vars.rs`'s stress tests, which run on every PR; this
container slice is representative, not exhaustive.
