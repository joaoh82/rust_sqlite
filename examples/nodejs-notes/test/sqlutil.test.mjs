import test from 'node:test';
import assert from 'node:assert/strict';

import { q, ident } from '../src/sqlutil.mjs';

test('q strings — basic and quote-doubling', () => {
  assert.equal(q('hello'), "'hello'");
  assert.equal(q("it's"), "'it''s'");
  assert.equal(q("a'b'c"), "'a''b''c'");
  assert.equal(q(''), "''");
});

test('q numbers — ints, floats, throws on NaN/Inf', () => {
  assert.equal(q(0), '0');
  assert.equal(q(42), '42');
  assert.equal(q(-7), '-7');
  assert.equal(q(1.5), '1.5');
  assert.throws(() => q(NaN), TypeError);
  assert.throws(() => q(Infinity), TypeError);
});

test('q booleans + null', () => {
  assert.equal(q(true), 'TRUE');
  assert.equal(q(false), 'FALSE');
  assert.equal(q(null), 'NULL');
  assert.equal(q(undefined), 'NULL');
});

test('q vector — bracket-array literal', () => {
  assert.equal(q([0.1, 0.2, 0.3]), '[0.1, 0.2, 0.3]');
  assert.equal(q([]), '[]');
  assert.throws(() => q([0.1, 'x']), TypeError);
  assert.throws(() => q([NaN]), TypeError);
});

test('q rejects objects', () => {
  assert.throws(() => q({}), TypeError);
});

test('ident — accepts only the engine\'s unquoted-identifier subset', () => {
  assert.equal(ident('users'), 'users');
  assert.equal(ident('_x9'), '_x9');
  assert.throws(() => ident('1users'), TypeError);
  assert.throws(() => ident('users; DROP TABLE x'), TypeError);
  assert.throws(() => ident('hello world'), TypeError);
  assert.throws(() => ident(''), TypeError);
});
