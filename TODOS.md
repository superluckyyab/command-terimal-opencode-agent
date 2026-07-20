# TODOS

## Wireline

### Caret-following popover positioning for the `@` picker

**What:** Make the `@` mention picker open near the actual cursor position (Slack/Discord/IDE-style), instead of the composer's existing static block-above-textarea placement.

**Why:** The multi-model + `@`-mention picker design deliberately reused the existing slash-popover's static positioning for its first version — fine for a leading `/`, but for a mid-sentence `@` the popover can end up visually far from where the user is actually typing.

**Context:** Requires an off-screen mirror-div measurement technique to get pixel coordinates for a caret inside a `<textarea>` — there's no native DOM API for this. Confirmed as an accepted, deliberate tradeoff in the original design (not an oversight) — revisit only if the static positioning turns out to actually bother daily use.

**Effort:** M
**Priority:** P3
**Depends on:** The `@`-mention picker itself (kaifa-main-design-20260720-133936.md) shipping first.

### Frontend test framework (zero automated coverage today)

**What:** Introduce a frontend test framework (e.g. vitest) for `Wireline App.dc.html` (~5000 lines, currently zero automated tests — manual/visual verification only).

**Why:** This project's `/plan-eng-review` pass on the multi-model + `@`-mention design surfaced two real bugs (a routing-sentinel value that would silently disappear from a dropdown, a regex anchored to the wrong scope) that were only caught by a human/AI reading the code line by line — nothing catches a regression here automatically.

**Context:** Project-wide concern, not specific to any one feature. Would need a build/test toolchain introduced (currently `build.js` just compiles the DC template to `dist/`, no bundler/test runner) and likely some refactoring to make the monolithic `Component` class's methods testable in isolation. Large, cross-cutting effort — not something to start inside a feature PR.

**Effort:** XL
**Priority:** P3
**Depends on:** None

## Completed

### Real MCP discovery/listing (`@` picker + `/mcp` now show real opencode MCP servers)

**What:** Wireline spawns a background `opencode serve` (`opencode_serve_ensure` in main.rs, fixed port 47823) and reads `GET /mcp` (see `loadMcps()`/`mcpStatusDesc()`), so the `@` picker and `/mcp` list real, locally-configured MCP servers and their live status (connected/failed/needs_auth/etc) — replaces the old hardcoded filesystem/kubernetes/prometheus/github demo list. Also fixed: the picker no longer intercepts `@` while in native-opencode mode (that composer pipes straight into the real opencode PTY).

**Resolution note:** Actually *calling* a specific MCP's tools mid-conversation turned out not to need any further Wireline code — it's a property of which MCP server is configured, not something Wireline mediates. The user's original `ssh-mcp` package bakes host/user/password into its own launch args, so switching SSH targets needed an `opencode.json` edit + restart; `@renqf/sshmcp` (every operation tool takes a `serverId`, servers managed live via its own GUI on port 3789, no opencode restart) is a better fit — an opencode-side config swap, zero Wireline changes. Opencode's own agent already auto-invokes tools from connected/enabled MCPs when relevant, standard MCP behavior, no explicit `@mention` required.

**Completed:** v1.27.0 (2026-07-20)
