import assert from 'node:assert/strict';
import test from 'node:test';
import { sanitizeExternalError } from './redaction.mjs';

test('redacts proxy credentials while preserving the error class and host', () => {
  const proxy = 'https://name%40tenant:p%40ss%2Fword@proxy.example:8443';
  const basic = Buffer.from('name@tenant:p@ss/word', 'utf8').toString('base64');
  const error = {
    class: 'ProxyConnectionError',
    message: `ProxyConnectionError through ${proxy}; decoded=name@tenant:p@ss/word password=p@ss/word encoded=name%40tenant:p%40ss%2Fword Basic ${basic}`,
  };
  const rendered = JSON.stringify(sanitizeExternalError(error, proxy));
  for (const secret of [proxy, 'name@tenant:p@ss/word', 'p@ss/word', 'name%40tenant:p%40ss%2Fword', 'p%40ss%2Fword', basic]) {
    assert.equal(rendered.includes(secret), false);
  }
  assert.equal(rendered.includes('ProxyConnectionError'), true);
  assert.equal(rendered.includes('proxy.example:8443'), true);
});
