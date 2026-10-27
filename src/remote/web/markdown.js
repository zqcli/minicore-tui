// Markdown as plain token trees, never HTML; the UI builds DOM nodes from them.
const SAFE_URL = /^https?:\/\//i;
function inline(s) {
  const out = [];
  let buf = '', i = 0, m;
  const flush = () => { if (buf) out.push({ t: 'text', s: buf }); buf = ''; };
  while (i < s.length) {
    const c = s[i];
    if (c === '\\' && /^[\\`*_{}[\]()#+\-.!|<>~]$/.test(s[i + 1] || '')) { buf += s[i + 1]; i += 2; }
    else if (c === '`') {
      let n = 1;
      while (s[i + n] === '`') n++;
      const e = s.indexOf('`'.repeat(n), i + n);
      if (e < 0) { buf += '`'.repeat(n); i += n; } else { flush(); out.push({ t: 'code', s: s.slice(i + n, e) }); i = e + n; }
    } else if (c === '*' || c === '_' || (c === '~' && s[i + 1] === '~')) {
      const dbl = s[i + 1] === c, w = dbl ? 2 : 1, e = s.indexOf(c.repeat(w), i + w);
      const open = s[i + w] && !/\s/.test(s[i + w]) && (c !== '_' || !/\w/.test(s[i - 1] || ''));
      if (open && e > i + w && !/\s/.test(s[e - 1]) && (c !== '_' || !/\w/.test(s[e + w] || ''))) {
        flush(); out.push({ t: c === '~' ? 's' : dbl ? 'b' : 'i', c: inline(s.slice(i + w, e)) }); i = e + w;
      } else { buf += c; i++; }
    } else if (c === '[' && (m = /^\[([^\]]*)\]\(\s*([^)\s]+)(?:\s+"[^"]*")?\s*\)/.exec(s.slice(i)))) {
      flush();
      out.push(SAFE_URL.test(m[2]) ? { t: 'a', href: m[2], c: inline(m[1]) } : { t: 'text', s: m[1] });
      i += m[0].length;
    } else if (c === 'h' && !/\w/.test(s[i - 1] || '') && (m = /^https?:\/\/[^\s<>]+/i.exec(s.slice(i)))) {
      const u = m[0].replace(/[.,;:!?)\]'"]+$/, '');
      flush(); out.push({ t: 'a', href: u, c: [{ t: 'text', s: u }] }); i += u.length;
    } else { buf += c; i++; }
  }
  flush();
  return out;
}
const cells = l => l.trim().replace(/^\|/, '').replace(/\|$/, '').replace(/\\\|/g, '\0').split('|').map(x => x.trim().replace(/\0/g, '|'));
const TSEP = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;
// Flat items with indentation become nested lists.
function nest(items, k, ind) {
  const list = { t: 'list', ord: items[k].ord != null, start: items[k].ord || 1, items: [] };
  while (k < items.length && items[k].ind >= ind) {
    const it = items[k];
    if (it.ind > ind && list.items.length) { const r = nest(items, k, it.ind); list.items[list.items.length - 1].sub.push(r.list); k = r.k; }
    else if ((it.ord != null) !== list.ord) break;
    else { list.items.push({ c: inline(it.text), sub: [] }); k++; }
  }
  return { list, k };
}
// Top-level blocks of `lines`; `cps` collects [line, blocks so far] at possible block starts.
function parseLines(lines, cps) {
  const out = [], LI = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)/;
  let para = [], i = 0, m;
  const li = k => { const x = LI.exec(lines[k] || ''); return x && (k < lines.length - 1 || x[3].trim()) ? x : null; };
  const flush = () => { if (para.length) out.push({ t: 'p', c: inline(para.join('\n')) }); para = []; };
  while (i < lines.length) {
    const l = lines[i];
    if (cps && !para.length) cps.push([i, out.length]);
    if ((m = /^ {0,3}(`{3,}|~{3,})\s*([^\s`]*)/.exec(l))) {
      flush();
      const fence = m[1], body = [];
      for (i++; i < lines.length; i++) {
        const t = lines[i].trim();
        if (t.length >= fence.length && t === fence[0].repeat(t.length)) break;
        body.push(lines[i]);
      }
      out.push({ t: 'code', lang: m[2], text: body.join('\n') });
      i++;
    } else if ((m = /^ {0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/.exec(l))) {
      flush(); out.push({ t: 'h', n: m[1].length, c: inline(m[2]) }); i++;
    } else if (/^ {0,3}([-*_])( *\1){2,} *$/.test(l)) {
      flush(); out.push({ t: 'hr' }); i++;
    } else if (/^ {0,3}>/.test(l)) {
      flush();
      const q = [];
      for (; i < lines.length && /^ {0,3}>/.test(lines[i]); i++) q.push(lines[i].replace(/^ {0,3}> ?/, ''));
      out.push({ t: 'quote', c: parseMd(q.join('\n')) });
    } else if (li(i)) {
      flush();
      const items = [];
      while (i < lines.length) {
        if ((m = li(i))) { items.push({ ind: m[1].replace(/\t/g, '    ').length, ord: /\d/.test(m[2]) ? parseInt(m[2], 10) : null, text: m[3] }); i++; }
        else if (items.length && /^\s+\S/.test(lines[i])) { items[items.length - 1].text += '\n' + lines[i].trim(); i++; }
        else if (!lines[i].trim() && li(i + 1)) i++;
        else break;
      }
      for (let k = 0; k < items.length;) { const r = nest(items, k, items[k].ind); out.push(r.list); k = r.k; }
    } else if (l.includes('|') && TSEP.test(lines[i + 1] || '') && (lines[i + 1] || '').includes('-')) {
      flush();
      const align = cells(lines[i + 1]).map(x => /^:-+:$/.test(x) ? 'center' : /-:$/.test(x) ? 'right' : '');
      const head = cells(l).map(inline), rows = [];
      for (i += 2; i < lines.length && lines[i].includes('|') && lines[i].trim(); i++) rows.push(cells(lines[i]).map(inline));
      out.push({ t: 'table', align, head, rows });
    } else {
      if (l.trim()) para.push(l); else flush();
      i++;
    }
  }
  flush();
  return out;
}
const norm = src => src.replace(/\r\n?/g, '\n');
const parseMd = src => parseLines(norm(src).split('\n'), null);
// Streaming Markdown: blocks before the last stable checkpoint are kept and only the tail is parsed again.
function mdInc(c, src) {
  src = norm(src);
  if (!c || !src.startsWith(c.src)) c = { src: '', at: 0, head: [] };
  const lines = src.slice(c.at).split('\n'), cps = [], tail = parseLines(lines, cps);
  let cut = null;
  for (const cp of cps) if (cp[0] > 0 && cp[0] + 2 <= lines.length - 1) cut = cp;
  const next = { src, at: c.at, head: c.head };
  if (cut) {
    let off = 0;
    for (let k = 0; k < cut[0]; k++) off += lines[k].length + 1;
    next.at += off;
    next.head = c.head.concat(tail.slice(0, cut[1]));
  }
  return { blocks: c.head.concat(tail), cache: next };
}

export { norm, inline, parseMd, mdInc };
