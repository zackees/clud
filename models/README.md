# Model Pages publisher

`manifest.py` defines the small model document and strict stable-family
selection. `publish.py` merges public OpenRouter and models.dev observations
with the last published document, then stages a complete
Pages artifact without rebuilding the installer history, and `preserve.py`
keeps the current model document when the installer site is rebuilt. The
contract and runtime consumer are in [model-pages.md](../docs/architecture/model-pages.md).
