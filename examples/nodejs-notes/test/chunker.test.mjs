import test from 'node:test';
import assert from 'node:assert/strict';

import {
  stripFrontmatter,
  deriveTitle,
  chunkMarkdown,
  approxTokens,
} from '../src/chunker.mjs';

test('stripFrontmatter — YAML between --- fences', () => {
  const text = '---\ntitle: Foo\ntags: [a]\n---\n\nBody.';
  const { frontmatter, body } = stripFrontmatter(text);
  assert.match(frontmatter, /^title: Foo/);
  assert.equal(body.trim(), 'Body.');
});

test('stripFrontmatter — no frontmatter passes through', () => {
  const text = '# Heading\n\nBody.';
  const { frontmatter, body } = stripFrontmatter(text);
  assert.equal(frontmatter, '');
  assert.equal(body, text);
});

test('deriveTitle — picks frontmatter title first', () => {
  assert.equal(
    deriveTitle({ frontmatter: 'title: Hello World', body: '# Other', fallback: 'fb' }),
    'Hello World',
  );
});

test('deriveTitle — falls back to first heading', () => {
  assert.equal(
    deriveTitle({ frontmatter: '', body: '# My Heading\n\nbody.', fallback: 'fb' }),
    'My Heading',
  );
});

test('deriveTitle — falls back to filename stem', () => {
  assert.equal(deriveTitle({ frontmatter: '', body: 'no heading', fallback: 'fb' }), 'fb');
});

test('approxTokens — whitespace word count', () => {
  assert.equal(approxTokens(''), 0);
  assert.equal(approxTokens('one two three'), 3);
  assert.equal(approxTokens('   a   b   '), 2);
});

test('chunkMarkdown — single short doc fits in one chunk', () => {
  const out = chunkMarkdown('# Title\n\nA short paragraph.\n');
  assert.equal(out.length, 1);
  assert.match(out[0].content, /Title/);
  assert.match(out[0].content, /short paragraph/);
});

test('chunkMarkdown — long doc splits into multiple chunks', () => {
  const big = Array.from({ length: 20 }, (_, i) => `Paragraph ${i}: ${'word '.repeat(60)}`).join(
    '\n\n',
  );
  const out = chunkMarkdown(`# Heading\n\n${big}`, { targetTokens: 200, overlapTokens: 20 });
  assert.ok(out.length > 1, `expected >1 chunk, got ${out.length}`);
  // Each chunk should be non-empty.
  for (const c of out) {
    assert.ok(c.content.length > 0);
  }
});

test('chunkMarkdown — overlap copies tail tokens forward', () => {
  const body =
    '# A\n\n' +
    Array.from({ length: 5 }, (_, i) => `alpha${i} ${'lorem '.repeat(80)}`).join('\n\n');
  const out = chunkMarkdown(body, { targetTokens: 100, overlapTokens: 30 });
  assert.ok(out.length >= 2);
  // The second chunk should contain the tail of the first.
  const firstTail = out[0].content.split(/\s+/).slice(-20).join(' ');
  assert.ok(
    out[1].content.includes(firstTail.split(' ').slice(-5).join(' ')),
    'second chunk should overlap with the tail of the first',
  );
});

test('chunkMarkdown — empty body produces no chunks', () => {
  assert.deepEqual(chunkMarkdown(''), []);
  assert.deepEqual(chunkMarkdown('   \n\n  '), []);
});
