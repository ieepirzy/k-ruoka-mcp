# Provider contracts

The combined server should normalize only the catalogue concepts that are genuinely shared.

## Shared read-only concepts

A product search result should be able to represent:

- provider / chain
- provider-native product id (`ean` for grocery chains, SKU for Alko)
- name
- price
- comparison/unit price where available
- image URL where available
- store-scoped availability where the provider exposes it

A store result should be able to represent:

- provider / chain
- provider-native store id
- name
- city/location
- provider-specific capabilities when relevant

## Explicit provider capabilities

Do not force these into a fake common denominator:

- K-Ruoka account login
- K-Ruoka cart mutation
- K-Ruoka OmaPlussa offers
- provider-specific offer/account surfaces added later

The MCP tool surface may offer convenient generic catalogue tools while retaining explicit chain-specific tools for richer capabilities.
