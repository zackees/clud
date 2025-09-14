# Safe defaults: firewall on, sudo enabled, no ssh keys
clud .

# Skip permission prompts inside container
clud . --dangerous

# Disable firewall
clud . --no-firewall

# Disable sudo explicitly
clud . --no-sudo

# Mount SSH keys read-only for git push
clud . --ssh-keys

# Override image and profile
clud . --image ghcr.io/vendor/claude:latest --profile python
