# Multistore migration plan

This branch keeps the upstream K-Ruoka implementation as the canonical, deep provider and grows read-only catalogue access for other Finnish chains around it.

## Provider roles

- **K-Ruoka**: canonical implementation in `browser/`; keeps account login, cart mutation, OmaPlussa offers, store-scoped availability and pricing.
- **Alko**: read-only guest-session provider in `providers/alko.rs`; no account login, cart, checkout, or purchasing surface.
- **S-Kaupat**: next provider. Reimplement the observed persisted-GraphQL-query behaviour independently; do not copy source from the unlicensed `p18a/mcp-ruoka` repository.

## Transport boundary

- Preserve stdio for local MCP use.
- Add Streamable HTTP after provider composition is stable.
- External OAuth / client identity / authorization belongs in Origo. The grocery MCP should only need a private service-auth seam when exposed remotely.

## Integration sequence

1. Compile and exercise the Alko provider behind a temporary dedicated MCP serve mode.
2. Add S-Kaupat catalogue/store discovery with persisted-query hash recovery.
3. Compose provider tool routers into one server while preserving K-Ruoka cart/login tools.
4. Normalize catalogue search inputs/outputs across chains where semantics genuinely match; keep chain-specific capabilities explicit where they do not.
5. Add Streamable HTTP for Origo-facing deployment.
6. Remove temporary provider-specific serve modes once the combined server is validated.

## Safety / scope

Checkout remains out of scope. Nothing added by the multistore work should place an order or spend money.
