#!/usr/bin/env node
// ponytail: real assertions for the two trickiest pieces of logic added by
// the multi-model + @-mention picker feature (kaifa-main-design-20260720-133936.md) —
// caret backward-scan trigger detection, and the old-config → chatModels[]
// migration (including the 'opencode' routing-sentinel exclusion the
// cross-model review caught). No test framework exists in this repo (see
// TODOS.md), and there's no Tauri/browser runtime available in dev here, so
// this extracts the pure(ish) methods straight out of the Component class
// source and runs them standalone. Run after touching either method:
//   node check-model-picker.js
const fs = require('fs');
const path = require('path');

const file = path.join(__dirname, 'Wireline App.dc.html');
const html = fs.readFileSync(file, 'utf8');
const start = html.indexOf('data-dc-script');
const tagEnd = html.indexOf('>', start) + 1;
const scriptEnd = html.indexOf('</script>', tagEnd);
const code = html.slice(tagEnd, scriptEnd);
const classStart = code.indexOf('class Component extends DCLogic');
const classSrc = code.slice(classStart);

function extractMethod(name) {
  const marker = '\n  ' + name + '(';
  const idx = classSrc.indexOf(marker);
  if (idx < 0) throw new Error('method not found: ' + name);
  const openParen = idx + marker.length - 1;
  const braceOpen = classSrc.indexOf('{', openParen);
  let i = braceOpen + 1, depth = 1;
  while (depth > 0) {
    if (classSrc[i] === '{') depth++;
    else if (classSrc[i] === '}') depth--;
    i++;
  }
  const params = classSrc.slice(openParen + 1, classSrc.indexOf(')', openParen));
  const body = classSrc.slice(braceOpen + 1, i - 1);
  return new Function(params, body);
}

let passed = 0, failed = 0;
function assertEq(actual, expected, label) {
  const a = JSON.stringify(actual), e = JSON.stringify(expected);
  if (a === e) { passed++; }
  else { failed++; console.error('FAIL: ' + label + ' — got ' + a + ', want ' + e); }
}

// --- findAtTrigger(text, caret, ch) ---
const findAtTrigger = extractMethod('findAtTrigger');
assertEq(findAtTrigger('@', 1, '@'), { start: 0, token: '' }, 'bare @ at end');
assertEq(findAtTrigger('hello @fo', 9, '@'), { start: 6, token: 'fo' }, 'mid-word after @');
assertEq(findAtTrigger('hello @foo bar', 15, '@'), null, 'closed token (space after) does not trigger');
assertEq(findAtTrigger('connect to @prod-server and check', 23, '@'), { start: 11, token: 'prod-server' }, 'mid-sentence @ with hyphen');
assertEq(findAtTrigger('no trigger here', 16, '@'), null, 'no @ at all');
assertEq(findAtTrigger('@a @b', 2, '@'), { start: 0, token: 'a' }, 'caret right after first token, second @ ignored');
assertEq(findAtTrigger('/model', 6, '/'), { start: 0, token: 'model' }, 'trigger char is configurable');

// --- migrateModelConfig(m) ---
const migrateModelConfig = extractMethod('migrateModelConfig');
const fakeThis = { uid: (p) => p + '-fixedid' };
const mig1 = migrateModelConfig.call(fakeThis, { apiBase: 'https://api.openai.com/v1', apiKey: 'sk-abc', model: 'opencode', chatModel: 'gpt-4o' });
assertEq(mig1.chatModels.length, 1, 'migration with real creds produces one entry');
assertEq(mig1.chatModels[0].primary, true, 'migrated entry is primary');
assertEq(mig1.chatModels[0].model, 'gpt-4o', 'migrated entry keeps chatModel as the model name');
assertEq(mig1.model, 'opencode', 'opencode sentinel preserved through migration');

const mig2 = migrateModelConfig.call(fakeThis, { apiBase: '', apiKey: '', model: 'opencode' });
assertEq(mig2.chatModels, [], 'empty apiBase/apiKey never creates a fake profile');
assertEq(mig2.model, 'opencode', 'sentinel preserved on empty config too');

const mig3 = migrateModelConfig.call(fakeThis, { apiBase: 'https://x', apiKey: 'k', model: 'glm-4-plus' });
assertEq(mig3.model, 'opencode', 'a stale custom model name (old __add__ flow) falls back to opencode, not a dangling id');

const mig4 = migrateModelConfig.call(fakeThis, { apiBase: 'https://x', apiKey: 'k', model: '__chat__' });
assertEq(mig4.model, '__chat__', '__chat__ sentinel preserved through migration');

// --- mcpStatusDesc(st) — real opencode McpStatus union (types.gen.d.ts) ---
// Verified live against a real `opencode serve` + GET /mcp during development
// (empty-config response parsed to []); these are the structurally-accurate
// shapes for the populated case, including the exact "connected" status the
// real embedded-ssh-agent MCP reports.
const mcpStatusDesc = extractMethod('mcpStatusDesc');
assertEq(mcpStatusDesc({ status: 'connected' }), 'MCP · connected', 'connected status');
assertEq(mcpStatusDesc({ status: 'disabled' }), 'MCP · disabled', 'disabled status');
assertEq(mcpStatusDesc({ status: 'needs_auth' }), 'MCP · needs auth', 'needs_auth status');
assertEq(mcpStatusDesc({ status: 'needs_client_registration' }), 'MCP · needs registration', 'needs_client_registration status');
assertEq(mcpStatusDesc({ status: 'failed', error: 'connection refused' }), 'MCP · failed: connection refused', 'failed status includes the error');
assertEq(mcpStatusDesc({}), 'MCP server', 'missing status falls back to a generic label, not a crash');

console.log(passed + ' passed, ' + failed + ' failed');
if (failed > 0) process.exit(1);
