Updated Prompt: Git Worktree Creation Inside Docker Container

Background / Problem
Making the workspace folder directly on the host has proven problematic.
However, the host’s Git repository is already volume-mapped into the container (normally at /host), so the container already has full access to the repository’s .git directory.

Goal
Design the procedure for creating a Git worktree entirely inside the container, using the existing host-mapped .git directory, while preserving the read-write nature of the repository volume and exposing the new worktree back to the host.

Requirements & Constraints

Repository Volume

The host project root (which contains .git) is already mapped as a read-write volume inside the container, usually at:

/host


This mapping must remain read-write so Git can update its metadata.

Worktree Target Directory

The host must provide a dedicated directory for the new worktree, for example:

project_root/worktree


Map this host directory into the container read-only at (for example):

/working


If project_root/worktree does not exist on the host, create it first:

mkdir -p project_root/worktree


Inside-Container Operation

All git worktree commands will be run inside the container, not on the host.

From inside the container:

cd /host
git worktree add /working my-branch


Replace my-branch with the branch name to check out, or add -b new-branch to create one.

Git Metadata Handling

Git stores worktree metadata in:

/host/.git/worktrees/<branch-name>/


Only the .git directory inside /host is updated.

The /working folder contains a normal checkout of the target branch, and the host sees it at:

project_root/worktree


Lifecycle & Cleanup

To remove the worktree later:

git worktree remove /working


(run from inside the container).

If the container is discarded before cleanup, you can also prune stale entries:

git worktree prune


Deliverable
Provide a step-by-step implementation or automation script that:

Ensures project_root/worktree exists on the host (creating it if necessary),

Maps it read-only into the container at /working,

Executes the git worktree add command from /host inside the container,

Leaves the original repository mapping writable, and

Documents cleanup procedures.

This updated prompt captures the new constraints—worktree creation inside the container, .git access via the existing /host mapping, and a read-only host directory for the worktree output—so that an agent or automation can implement it without relying on direct host operations.