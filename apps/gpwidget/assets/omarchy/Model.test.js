// Pure presentation/protocol boundary checks; transport is exercised by the
// real Quickshell harness in tests/omarchy/shell.qml.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const model = {};
vm.createContext(model);
vm.runInContext(fs.readFileSync(`${__dirname}/Model.js`, 'utf8').replace(/^\.pragma library\s*/, ''), model);
let checks = 0;
function equal(actual, expected) {
  assert.deepEqual(JSON.parse(JSON.stringify(actual)), expected);
  checks++;
}
for (const state of model.states) assert(!model.stateLabel(state).startsWith('Unknown'));
equal(model.stateLabel('future'), 'Unknown state · future');
for (const value of [null, [], {}, 'connected', { state: 1 }, { state: '' }]) equal(!!model.validStatus(value), false);
equal(!!model.validStatus({ state: 'future' }), true);
equal(model.gateways([{ name: 'AU', address: 'au.example' }, null, { name: 'bad' }, { name: '', address: 'fallback' }]),
  [{ value: 'au.example', label: 'AU' }, { value: 'fallback', label: 'fallback' }]);
equal(model.gateways({}), []);
for (const value of ['false', '0', 0, false, ' no ', 'off', '']) equal(model.boolSetting(value, true), false);
for (const value of ['true', '1', 1, true, 'yes', 'on']) equal(model.boolSetting(value, false), true);
equal(model.boolSetting(undefined, false), false);
equal(model.boolSetting(null, true), true);
equal(model.errorLine('INFO Starting VPN\nError: authorization denied\n'), 'authorization denied');
equal(model.errorLine('\n'), '');
equal(model.duration(-5), '0s');
equal(model.duration(0), '0s');
equal(model.duration(65), '1m 5s');
equal(model.duration(3661), '1h 1m');
equal(model.duration(90000), '1d 1h');
equal(model.bytes(0), '0 B');
equal(model.bytes(-1), '0 B');
equal(model.bytes(1024), '1.00 KiB');
equal(model.bytes(1024 * 1024 * 12.5), '12.5 MiB');
equal(model.bytes(Infinity), '—');
console.log(`${checks} model checks passed`);
