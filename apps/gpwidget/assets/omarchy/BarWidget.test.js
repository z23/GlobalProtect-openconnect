// Tests for the Omarchy bar widget's logic.
//
//   node apps/gpwidget/assets/omarchy/BarWidget.test.js [path/to/BarWidget.qml]
//   make test-omarchy-widget
//
// Quickshell's QML can't be driven headlessly — Process is gated on the engine's
// reload hook and the `qs.*` modules are synthesised at runtime — so instead of
// mocking a shell, this extracts the real functions and the real
// statusProcess.onExited handler out of BarWidget.qml as text and runs them
// against stand-ins for the QML objects they touch. The upside is that the test
// cannot drift from what ships; the limit is that it verifies logic, not
// rendering or process lifecycle.
//
// Cover the handlers, not just the helpers. An earlier version of this file
// tested the helper functions in isolation and passed cleanly while the widget
// was broken outright, because `applySnapshot(raw, value)` and
// `applySnapshot(value)` are indistinguishable until something calls them.
//
// The mirrors in makeCtx() (vpnState, faulted, dimmed, ...) restate QML property
// bindings by hand and must be updated alongside them.
const fs = require('fs');
const path = require('path');

const SRC_PATH = process.argv[2] || path.join(__dirname, 'BarWidget.qml');
const src = fs.readFileSync(SRC_PATH, 'utf8');

function matchBraces(from) {
  let depth = 0, j = src.indexOf('{', from);
  for (; j < src.length; j++) {
    if (src[j] === '{') depth++;
    else if (src[j] === '}') { depth--; if (depth === 0) return j + 1; }
  }
  throw new Error('unbalanced braces');
}

function extractFunction(name) {
  const start = src.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`function ${name} not found`);
  return src.slice(start, matchBraces(start));
}

// The body of statusProcess.onExited, as an invocable function.
function extractOnExited() {
  const anchor = src.indexOf('onExited: function(exitCode) {');
  if (anchor < 0) throw new Error('statusProcess.onExited not found');
  const block = src.slice(anchor, matchBraces(anchor));
  return block.replace(/^onExited:\s*/, '');
}

function extractIntervalExpr() {
  const m = src.match(/interval: \{\s*\n([\s\S]*?)\n    \}/);
  if (!m) throw new Error('poll timer interval binding not found');
  return m[1];
}

const FN_NAMES = ['boolSetting', 'errorLine', 'snapshotKeyOf', 'applySnapshot',
                  'setError', 'setActionError', 'stateLabel', 'label', 'tooltip'];
const bodies = FN_NAMES.map(extractFunction).join('\n');
const knownStates = eval(src.match(/readonly property var knownStates: (\[[\s\S]*?\])/)[1]);

function makeCtx(over) {
  const ctx = Object.assign({
    settings: {}, snapshot: { state: 'stack-down' }, snapshotKey: '',
    actionError: '', actionErrorState: '', vertical: false,
  }, over);

  Object.defineProperties(ctx, {
    vpnState:     { get: () => String(ctx.snapshot.state || 'stack-down') },
    connected:    { get: () => ctx.vpnState === 'connected' },
    busy:         { get: () => ['authenticating', 'connecting', 'disconnecting'].includes(ctx.vpnState) },
    unknownState: { get: () => knownStates.indexOf(ctx.vpnState) === -1 },
    faulted:      { get: () => ctx.vpnState === 'error' || ctx.unknownState || ctx.actionError !== '' },
    gatewayName:  { get: () => ctx.snapshot.gateway && ctx.snapshot.gateway.name ? String(ctx.snapshot.gateway.name) : '' },
    showGateway:  { get: () => ctx.boolSetting('showGateway', true) },
    knownStates:  { get: () => knownStates },
    // WidgetButton: opacity is 0.45 when dimmed, and the label uses the urgent
    // accent when active. Mirrors the two bindings on the button.
    dimmed:       { get: () => !ctx.faulted && ['stack-down', 'disconnected', 'needs-setup'].includes(ctx.vpnState) },
    active:       { get: () => ctx.faulted },
  });

  ctx.setting = (n, fb) => (ctx.settings[n] === undefined || ctx.settings[n] === null ? fb : ctx.settings[n]);
  ctx.root = ctx;

  // Stand-ins for the QML objects the handler touches.
  ctx.statusProcess = { sawExit: false, timedOut: false, running: false };
  ctx.statusWatchdog = { stop() {}, restart() {} };
  ctx.statusKill = { stop() {}, restart() {} };
  ctx.statusStdout = { text: '' };
  ctx.statusStderr = { text: '' };

  Object.assign(ctx, new Function('ctx', `with (ctx) { ${bodies}; return { ${FN_NAMES.join(', ')} }; }`)(ctx));
  ctx.onExited = new Function('ctx', `with (ctx) { return ${extractOnExited()}; }`)(ctx);
  return ctx;
}

let pass = 0, fail = 0;
// `actual` is passed as a thunk so a broken widget throws into the failure
// report instead of aborting the run — the whole point is to see every failure.
function check(desc, actual, expected) {
  let got;
  try { got = typeof actual === 'function' ? actual() : actual; }
  catch (e) { fail++; console.log(`FAIL  ${desc}\n      threw    ${e.message}`); return; }
  if (JSON.stringify(got) === JSON.stringify(expected)) pass++;
  else { fail++; console.log(`FAIL  ${desc}\n      expected ${JSON.stringify(expected)}\n      actual   ${JSON.stringify(got)}`); }
}

// ================= REGRESSION GUARD: the onExited call site =================
// A healthy daemon payload must reach `snapshot` as a parsed object.
{
  const c = makeCtx({});
  c.statusStdout.text = JSON.stringify({ state: 'connected', gateway: { name: 'go.example' }, conn: { rxBytes: 1 } });
  c.onExited(0);
  check('healthy poll yields an object snapshot', typeof c.snapshot, 'object');
  check('healthy poll sets vpnState', c.vpnState, 'connected');
  check('healthy poll sets gateway', () => c.gatewayName, 'go.example');
  check('healthy poll renders connected', c.label(), 'VPN  go.example');
  check('healthy poll is not dimmed', c.dimmed, false);
  check('healthy poll is not faulted', c.faulted, false);

  // ...and a second poll whose rendered fields are unchanged must not freeze it.
  c.statusStdout.text = JSON.stringify({ state: 'connected', gateway: { name: 'go.example' }, conn: { rxBytes: 999 } });
  c.onExited(0);
  check('counter-only change deduped', () => c.snapshot.conn.rxBytes, 1);
  c.statusStdout.text = JSON.stringify({ state: 'disconnected' });
  c.onExited(0);
  check('real state change still applies', c.vpnState, 'disconnected');
  check('disconnected dims', c.dimmed, true);
}
{
  const c = makeCtx({});
  c.statusStdout.text = '';
  c.onExited(0);
  check('empty stdout -> error', c.vpnState, 'error');
  check('empty stdout message', () => c.snapshot.error, 'Empty status response');
}
{
  const c = makeCtx({});
  c.statusStdout.text = 'not json';
  c.onExited(0);
  check('malformed stdout -> error', c.snapshot.error, 'Invalid status response');
}
{
  const c = makeCtx({});
  c.statusStdout.text = JSON.stringify([1, 2]);
  c.onExited(0);
  check('array payload rejected', c.snapshot.error, 'Invalid status response');
}
{
  const c = makeCtx({});
  c.statusStdout.text = JSON.stringify({ gateway: { name: 'x' } });
  c.onExited(0);
  check('stateless payload rejected', c.snapshot.error, 'Invalid status response');
}
{
  const c = makeCtx({});
  c.statusStderr.text = 'Error: Failed to connect to /run/user/1000/gpwidget.sock: Permission denied';
  c.onExited(1);
  check('non-zero exit surfaces stderr cause', c.snapshot.error, 'Failed to connect to /run/user/1000/gpwidget.sock: Permission denied');
}
{
  const c = makeCtx({});
  c.statusStderr.text = '';
  c.onExited(1);
  check('non-zero exit w/o stderr falls back', c.snapshot.error, 'gpwidget status failed');
}
{
  const c = makeCtx({});
  c.statusProcess.timedOut = true;
  c.statusStderr.text = 'irrelevant';
  c.onExited(15);
  check('watchdog kill does not overwrite timeout msg', c.snapshotKey, '');
}

// ================= errorLine: real gpwidget stderr shapes =================
const el = makeCtx({}).errorLine;
check('picks trailing Error: over leading INFO banner',
  el('[2026-08-11T05:00:00Z INFO  gpwidget::launch] Starting VPN service stack via /usr/bin/gpclient\n' +
     'Error: VPN service did not come up within 30s — polkit authorization may have been denied'),
  'VPN service did not come up within 30s — polkit authorization may have been denied');
check('bare Error: line', el('Error: boom'), 'boom');
check('no Error: line -> last non-empty', el('one\ntwo\n\n'), 'two');
check('empty', el(''), '');
check('whitespace only', el('  \n \n'), '');
check('null', el(null), '');
check('long is capped at 120', el('Error: ' + 'x'.repeat(300)).length, 120);
check('long ends in ellipsis', el('Error: ' + 'x'.repeat(300)).slice(-1), '…');

// ================= actionError lifecycle =================
{
  const c = makeCtx({});
  c.setActionError('polkit denied');
  check('actionError faults', c.faulted, true);
  check('actionError records state', c.actionErrorState, 'stack-down');
  check('faulted is NOT dimmed (urgent stays full opacity)', c.dimmed, false);
  check('faulted is active', c.active, true);
  check('actionError in tooltip', c.tooltip().split('\n')[1], 'Last action failed · polkit denied');
  check('actionError marks the bar', c.label(), 'VPN  !');

  // A failed-but-effective disconnect lands on `disconnected`, not connected/busy.
  c.snapshot = { state: 'disconnected' };
  if (c.actionError !== '' && c.vpnState !== c.actionErrorState) { c.actionError = ''; c.actionErrorState = ''; }
  check('actionError clears on move to disconnected', c.actionError, '');
  check('no longer faulted', c.faulted, false);
  check('now dims normally', c.dimmed, true);
}

// ================= settings coercion =================
const bs = (v) => makeCtx({ settings: v === undefined ? {} : { showGateway: v } }).boolSetting('showGateway', true);
for (const [v, want] of [[true, true], [false, false], ['true', true], ['false', false], ['False', false],
                         ['0', false], ['no', false], ['off', false], ['OFF', false], ['', false],
                         [1, true], [0, false], [undefined, true], [null, true]])
  check(`boolSetting ${JSON.stringify(v)}`, bs(v), want);

// ================= interval coercion =================
const intervalExpr = extractIntervalExpr();
const intervalFor = (v) => {
  const c = makeCtx({ settings: v === undefined ? {} : { refreshIntervalSec: v } });
  return new Function('ctx', `with (ctx) { return (function(){ ${intervalExpr} })(); }`)(c);
};
for (const [v, want] of [[undefined, 3000], [10, 10000], ['10', 10000], ['3s', 3000], ['abc', 3000],
                         [Infinity, 3000], [0, 1000], [-5, 1000], [Math.pow(2, 29), 3600000],
                         [1e12, 3600000]])
  check(`interval ${JSON.stringify(v)}`, intervalFor(v), want);

const probes = [undefined, 10, '10', '3s', 'abc', Infinity, -Infinity, 0, -5, null, '',
                Math.pow(2, 29), Math.pow(2, 31), 1e12, 1e300];
const asInt32 = (ms) => ms | 0;
check('no interval collapses to a 0ms hot loop after int coercion',
  probes.every(v => asInt32(intervalFor(v)) >= 1000), true);

// ================= state mapping =================
for (const s of knownStates) check(`"${s}" is known`, makeCtx({ snapshot: { state: s } }).unknownState, false);
const unk = makeCtx({ snapshot: { state: 'reconnecting' } });
check('unknown flagged', unk.unknownState, true);
check('unknown faulted', unk.faulted, true);
check('unknown label', unk.stateLabel(), 'Unknown state · reconnecting');
check('unknown not dimmed', unk.dimmed, false);
check('stack-down label', makeCtx({ snapshot: { state: 'stack-down' } }).stateLabel(), 'VPN off');
check('needs-setup label', makeCtx({ snapshot: { state: 'needs-setup' } }).stateLabel(), 'Setup required');
check('needs-setup dims', makeCtx({ snapshot: { state: 'needs-setup' } }).dimmed, true);
check('connected w/ gw', makeCtx({ snapshot: { state: 'connected', gateway: { name: 'g' } } }).stateLabel(), 'Connected · g');
check('connected w/o gw', makeCtx({ snapshot: { state: 'connected' } }).stateLabel(), 'Connected');
check('error w/ text', makeCtx({ snapshot: { state: 'error', error: 'nope' } }).stateLabel(), 'Error · nope');
check('vertical is icon-only', makeCtx({ snapshot: { state: 'connected', gateway: { name: 'g' } }, vertical: true }).label(), 'VPN');
check('showGateway off hides name', makeCtx({ snapshot: { state: 'connected', gateway: { name: 'g' } }, settings: { showGateway: false } }).label(), 'VPN');
check('busy shows ellipsis', makeCtx({ snapshot: { state: 'connecting' } }).label(), 'VPN  …');

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
