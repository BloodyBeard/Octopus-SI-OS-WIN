export function sanitizeExternalError(value, proxyUrl = '') {
  if (value instanceof Error) return sanitizeExternalError(String(value.stack || value.message || value), proxyUrl);
  if (Array.isArray(value)) return value.map(item => sanitizeExternalError(item, proxyUrl));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, sanitizeExternalError(item, proxyUrl)]));
  }
  if (typeof value !== 'string' || !proxyUrl) return value;
  let parsed;
  try {
    parsed = new URL(proxyUrl);
  } catch {
    return value.split(proxyUrl).join('[proxy-redacted]');
  }
  const rawAuthority = proxyUrl.slice(proxyUrl.indexOf('//') + 2).split(/[/?#]/u, 1)[0];
  if (!rawAuthority.includes('@')) return value;
  const encodedUserinfo = rawAuthority.slice(0, rawAuthority.lastIndexOf('@'));
  const authority = rawAuthority.slice(rawAuthority.lastIndexOf('@') + 1);
  let username = '';
  let password = '';
  try {
    username = decodeURIComponent(parsed.username || '');
    password = decodeURIComponent(parsed.password || '');
  } catch {}
  const decodedUserinfo = `${username}${parsed.password ? `:${password}` : ''}`;
  const redactedUrl = `${parsed.protocol}//[credentials-redacted]@${authority}${parsed.pathname}${parsed.search}${parsed.hash}`;
  let sentinel = '\u0000octopus-whatsapp-proxy-url\u0000';
  while (value.includes(sentinel)) sentinel += '_';
  let sanitized = value.split(proxyUrl).join(sentinel);
  const tokens = new Set([encodedUserinfo, decodedUserinfo]);
  if (decodedUserinfo) tokens.add(Buffer.from(decodedUserinfo, 'utf8').toString('base64'));
  for (const token of [parsed.username, parsed.password, username, password]) {
    if (token && token.length >= 3) tokens.add(token);
  }
  for (const token of [...tokens]) {
    if (!token) continue;
    tokens.add(encodeURIComponent(token));
    tokens.add(encodeURIComponent(token).replace(/%20/gu, '+'));
  }
  for (const token of [...tokens].filter(Boolean).sort((left, right) => right.length - left.length)) {
    sanitized = sanitized.split(token).join('[credentials-redacted]');
  }
  return sanitized.split(sentinel).join(redactedUrl);
}
