# Model Pages publisher

`manifest.py` defines the small model document and strict stable-family
selection. `publish.py` merges public OpenRouter and models.dev observations
with the last published document, then stages a complete
Pages artifact without rebuilding the installer history, and `preserve.py`
keeps the current model document when the installer site is rebuilt. The
contract and runtime consumer are in [model-pages.md](../docs/architecture/model-pages.md).

`selection-policy.json` lists model IDs the ChatGPT Codex backend has rejected.
The publisher removes these IDs from both fresh catalog observations and the
previously published manifest. This lets an operator roll back the server
default without requiring existing clud clients to upgrade, and prevents the
next nightly publication from undoing the rollback. Remove an ID only after a
live ChatGPT backend probe succeeds.
