# Origo boundary

For remote/shared deployment, Origo is the public authentication and authorization boundary.

```text
MCP client / agent
       |
       | OAuth 2.1 / client auth / future agent identity
       v
     Origo
       |
       | private service credential / trusted network seam
       v
 Finnish grocery MCP
       |
       +-- K-Ruoka browser-backed account integration
       +-- Alko read-only catalogue
       +-- S-Kaupat read-only catalogue
```

The grocery server should not grow a second external identity system. K-Plussa login is a separate downstream-account concern: it authenticates the server's browser session to K-Ruoka, not the external MCP caller.

Provider defaults that are personal or caller-specific should be supplied by Origo/caller context rather than stored as global shared-server state. A single-user deployment may still use the existing persisted K-Ruoka default store for convenience.
