# TODOS

## Wireline

### Real MCP protocol integration

**What:** Let `/mcp` actually discover and connect to locally-configured MCP servers, replacing `this.state.mcps`'s hardcoded demo list (kubernetes/prometheus/github/generic canned responses).

**Why:** Right now selecting `@kubernetes` etc. returns a scripted fake reply — no real value. The multi-model + `@`-mention picker design (kaifa-main-design-20260720-133936.md) depends on this shipping first and explicitly deferred real MCP wiring (premise 5) to keep that PR scoped.

**Context:** `this.state.mcps` and the `/mcp`/`@name` targeting UI (Wireline App.dc.html:3403-3454, 4581-4590) are in place and will be reused as-is — this TODO is purely about swapping the fake candidate list and canned responses for a real MCP client (discovery, connection management, auth). Substantial scope on its own; not a small follow-up.

**Effort:** L
**Priority:** P2
**Depends on:** None

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
