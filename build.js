#!/usr/bin/env node
// Assembles a self-contained, offline dist/ for the Tauri build.
// It inlines the Wireline app's <x-dc> markup into index.html and loads
// React from local vendor files (no CDN), so the packaged app works with
// no network. Run: node build.js
const fs = require('fs');
const path = require('path');

const root = __dirname;
const dist = path.join(root, 'dist');
const APP = 'Wireline App.dc.html';

const src = fs.readFileSync(path.join(root, APP), 'utf8');
// The source leaves <x-dc> implicitly closed (the browser auto-closes it at
// </body>). Take everything from <x-dc> up to </body> and close it explicitly.
const open = src.indexOf('<x-dc');
const bodyEnd = src.lastIndexOf('</body>');
if (open < 0 || bodyEnd < 0) { console.error('could not find <x-dc>/…/</body> in ' + APP); process.exit(1); }
const explicitClose = src.lastIndexOf('</x-dc>');
const xdc = explicitClose > open
  ? src.slice(open, explicitClose + '</x-dc>'.length)
  : src.slice(open, bodyEnd).replace(/\s*$/, '') + '\n</x-dc>';

const html = `<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Wireline</title>
<link rel="stylesheet" href="./vendor/xterm/xterm.css">
<script src="./vendor/react.production.min.js"></script>
<script src="./vendor/react-dom.production.min.js"></script>
<script src="./vendor/xterm/xterm.js"></script>
<script src="./vendor/xterm/addon-fit.js"></script>
<script src="./vendor/xterm/addon-search.js"></script>
<script src="./support.js"></script>
</head>
<body>
${xdc}
</body>
</html>
`;

function copy(rel) {
  const from = path.join(root, rel);
  const to = path.join(dist, rel);
  fs.mkdirSync(path.dirname(to), { recursive: true });
  fs.copyFileSync(from, to);
}

fs.rmSync(dist, { recursive: true, force: true });
fs.mkdirSync(dist, { recursive: true });
fs.writeFileSync(path.join(dist, 'index.html'), html);
copy('support.js');
copy('vendor/react.production.min.js');
copy('vendor/react-dom.production.min.js');
copy('vendor/xterm/xterm.js');
copy('vendor/xterm/addon-fit.js');
copy('vendor/xterm/addon-search.js');
copy('vendor/xterm/xterm.css');
copy('assets/broadsheet.css');
copy('_ds/broadsheet-842b0694-9893-4be7-89c5-dcb6c1fdef65/_ds_bundle.js');
copy('_ds/broadsheet-842b0694-9893-4be7-89c5-dcb6c1fdef65/styles.css');

console.log('built dist/ (index.html + support.js + vendor + assets)');
