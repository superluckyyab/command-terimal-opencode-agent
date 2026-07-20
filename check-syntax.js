#!/usr/bin/env node
// ponytail: syntax-only smoke check for the inline <script type="text/x-dc">
// in "Wireline App.dc.html" — this repo has no test framework (see TODOS.md),
// no Tauri/browser runtime is available in CI/dev here, so this is the
// cheapest signal that an edit didn't break the JS. Run after every change:
//   node check-syntax.js
const fs = require('fs');
const path = require('path');

const file = path.join(__dirname, 'Wireline App.dc.html');
const html = fs.readFileSync(file, 'utf8');
const start = html.indexOf('data-dc-script');
if (start < 0) { console.error('could not find the data-dc-script tag'); process.exit(1); }
const tagEnd = html.indexOf('>', start) + 1;
const scriptEnd = html.indexOf('</script>', tagEnd);
const code = html.slice(tagEnd, scriptEnd);

try {
  new Function(code);
  console.log('OK: inline script parses (' + code.length + ' chars)');
} catch (e) {
  console.error('SYNTAX ERROR:', e.message);
  process.exit(1);
}
