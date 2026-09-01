# Multistore branch notes

This branch is intentionally incremental. `k-ruoka-mcp serve` remains the unchanged K-Ruoka server while new providers are proven independently. `k-ruoka-mcp serve-alko` is a temporary read-only Alko surface used to validate the provider before router composition.

Do not publish this branch as a replacement release until the combined provider router and HTTP deployment path are complete.
