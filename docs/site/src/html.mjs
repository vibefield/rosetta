import { glyphs } from './glyphs.mjs';

export const escape = value => String(value).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
export function icon(name, extra = '') {
  const drawing = glyphs[name];
  if (!drawing) throw new Error(`Unknown Field Glyph: ${name}`);
  const shape = ([tag, attrs]) => `<${tag} ${Object.entries(attrs).map(([key, value]) => `${key}="${escape(value)}"`).join(' ')}/>`;
  const contents = drawing.kind === 'hardware'
    ? `<path fill="currentColor" fill-rule="evenodd" d="${drawing.d}"/>`
    : `<g fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${drawing.strokes.map(shape).join('')}</g><g fill="currentColor">${drawing.fills.map(shape).join('')}</g>`;
  return `<svg class="icon ${extra}" viewBox="0 0 24 24" aria-hidden="true" focusable="false">${contents}</svg>`;
}

// Highlight examples without executing them or shipping a highlighter runtime.
export function highlight(source, types = new Set(), url = () => '') {
  const re = /\/\*[\s\S]*?\*\/|\/\/[^\n]*|#[^\n]*|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`(?:\\.|[^`\\])*`|\b(?:export|import|from|type|interface|const|let|new|await|if|else|throw|return|async|function|try|catch|instanceof|void|readonly)\b|\b(?:true|false|null|undefined)\b|\b\d+(?:\.\d+)?\b|\b[A-Z][A-Za-z0-9_]*\b/g;
  let output = '', last = 0;
  for (const match of source.matchAll(re)) {
    output += escape(source.slice(last, match.index));
    const word = match[0];
    let cls = 'syntax-keyword';
    if (/^(\/\*|\/\/|#)/.test(word)) cls = 'syntax-comment';
    else if (/^["'`]/.test(word)) cls = 'syntax-string';
    else if (/^(\d|true|false|null|undefined)/.test(word)) cls = 'syntax-value';
    else if (/^[A-Z]/.test(word)) cls = 'syntax-type';
    output += types.has(word) ? `<a class="type-token" href="${url(`types/${word}`)}">${word}</a>` : `<span class="${cls}">${escape(word)}</span>`;
    last = match.index + word.length;
  }
  return output + escape(source.slice(last));
}

export function code(source, language = 'TypeScript', options = {}) {
  return `<div class="code-block${options.dark ? ' code-dark' : ''}"><div class="code-head"><span>${escape(language)}</span><button class="copy-button" type="button" aria-label="Copy ${escape(language)} example" data-copy>${icon('duplicate')}<span>Copy</span></button></div><pre tabindex="0"><code>${highlight(source, options.types, options.url)}</code></pre></div>`;
}

export function tabs(examples, id, options = {}) {
  return `<div class="code-tabs${options.dark ? ' code-dark' : ''}" data-tabs><div class="tab-bar" role="tablist" aria-label="Example language">${examples.map(([name], i) => `<button type="button" role="tab" id="${id}-tab-${i}" aria-controls="${id}-panel-${i}" aria-selected="${i === 0}" tabindex="${i === 0 ? 0 : -1}">${escape(name)}</button>`).join('')}<span class="code-caption">${escape(options.caption ?? 'Example request')}</span></div>${examples.map(([name, source], i) => `<div role="tabpanel" id="${id}-panel-${i}" aria-labelledby="${id}-tab-${i}" ${i ? 'hidden' : ''}>${code(source, name, options)}</div>`).join('')}</div>`;
}

export function table(headers, rows) {
  return `<div class="table-scroll" tabindex="0" role="region" aria-label="${escape(headers.join(', '))}"><table><thead><tr>${headers.map(header => `<th scope="col">${escape(header)}</th>`).join('')}</tr></thead><tbody>${rows.map(row => `<tr>${row.map(cell => `<td>${cell}</td>`).join('')}</tr>`).join('')}</tbody></table></div>`;
}

export const note = (title, body) => `<aside class="note">${icon('document')}<div><strong>${escape(title)}</strong><p>${body}</p></div></aside>`;
export const section = (id, title, contents) => `<section class="doc-section" aria-labelledby="${id}"><h2 id="${id}"><a href="#${id}">${escape(title)}<span aria-hidden="true">#</span></a></h2>${contents}</section>`;
export const inline = text => `<code>${escape(text)}</code>`;
export const sourceUrl = path => `https://github.com/vibefield/rosetta/blob/main/${path}`;
