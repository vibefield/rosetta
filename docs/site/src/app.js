const $ = (selector, root = document) => root.querySelector(selector);
const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
const themeButton = $('[data-theme-toggle]');
const systemTheme = matchMedia('(prefers-color-scheme: dark)');
function updateTheme(dark) {
  document.documentElement.classList.toggle('dark', dark);
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
  themeButton.setAttribute('aria-label', `Switch to ${dark ? 'light' : 'dark'} theme`);
  $('meta[name="theme-color"]').content = dark ? '#171717' : '#fafafa';
}
updateTheme(document.documentElement.classList.contains('dark'));
themeButton.addEventListener('click', () => {
  const dark = !document.documentElement.classList.contains('dark');
  updateTheme(dark);
  try { localStorage.setItem('rosetta-docs-theme', dark ? 'dark' : 'light'); } catch {}
});
systemTheme.addEventListener('change', event => {
  let saved;
  try { saved = localStorage.getItem('rosetta-docs-theme'); } catch {}
  if (!saved) updateTheme(event.matches);
});

let toastTimer;
function toast(message) {
  const target = $('.toast');
  clearTimeout(toastTimer);
  target.textContent = message;
  target.hidden = false;
  toastTimer = setTimeout(() => { target.hidden = true; }, 2400);
}
for (const button of $$('[data-copy]')) {
  button.addEventListener('click', async () => {
    const content = $('pre code', button.closest('.code-block')).textContent;
    try {
      await navigator.clipboard.writeText(content);
      $('span', button).textContent = 'Copied';
      button.dataset.copied = 'true';
      setTimeout(() => { $('span', button).textContent = 'Copy'; delete button.dataset.copied; }, 1800);
    } catch {
      const selection = window.getSelection();
      const range = document.createRange();
      range.selectNodeContents($('pre code', button.closest('.code-block')));
      selection.removeAllRanges();
      selection.addRange(range);
      toast('Code selected. Press ⌘C or Ctrl+C to copy.');
    }
  });
}

for (const group of $$('[data-tabs]')) {
  const buttons = $$('[role="tab"]', group);
  const select = index => {
    for (const [i, button] of buttons.entries()) {
      button.setAttribute('aria-selected', String(i === index));
      button.tabIndex = i === index ? 0 : -1;
      document.getElementById(button.getAttribute('aria-controls')).hidden = i !== index;
    }
  };
  for (const [index, button] of buttons.entries()) {
    button.addEventListener('click', () => select(index));
    button.addEventListener('keydown', event => {
      const offset = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
      if (!offset && !['Home', 'End'].includes(event.key)) return;
      event.preventDefault();
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : (index + offset + buttons.length) % buttons.length;
      select(next);
      buttons[next].focus();
    });
  }
}

const menuButton = $('[data-menu-toggle]');
const sidebar = $('#sidebar');
const scrim = $('[data-menu-close]');
const mobile = matchMedia('(max-width: 800px)');
function menu(open, restoreFocus = true) {
  document.body.classList.toggle('menu-open', open);
  menuButton.setAttribute('aria-expanded', String(open));
  menuButton.setAttribute('aria-label', open ? 'Close navigation' : 'Open navigation');
  scrim.hidden = !open;
  sidebar.inert = mobile.matches && !open;
  $('#main').inert = open;
  $('.page-rail').inert = open;
  $('.top-nav').inert = open;
  if (open) $('a[aria-current="page"]', sidebar)?.focus();
  else if (restoreFocus) menuButton.focus();
}
menu(false, false);
menuButton.addEventListener('click', () => menu(!document.body.classList.contains('menu-open')));
scrim.addEventListener('click', () => menu(false));
mobile.addEventListener('change', () => menu(false, false));
document.addEventListener('keydown', event => {
  if (!document.body.classList.contains('menu-open')) return;
  if (event.key === 'Escape') { event.preventDefault(); menu(false); }
  if (event.key === 'Tab') {
    const focusable = [menuButton, ...$$('a, button, summary', sidebar)].filter(el => el.getClientRects().length);
    const index = focusable.indexOf(document.activeElement);
    if (event.shiftKey && index <= 0) { event.preventDefault(); focusable.at(-1).focus(); }
    else if (!event.shiftKey && index === focusable.length - 1) { event.preventDefault(); focusable[0].focus(); }
  }
});

const filter = $('[data-filter]');
if (filter) {
  const items = $$('[data-filter-item]');
  filter.addEventListener('input', () => {
    const terms = filter.value.toLowerCase().trim().split(/\s+/).filter(Boolean);
    let visible = 0;
    for (const item of items) {
      item.hidden = !terms.every(term => item.dataset.search.includes(term));
      if (!item.hidden) visible++;
    }
    for (const group of $$('[data-filter-group]')) group.hidden = !$$('[data-filter-item]', group).some(item => !item.hidden);
    const noun = document.documentElement.dataset.page === 'types' ? 'type' : 'endpoint';
    $('[data-filter-count]').textContent = `${visible} ${noun}${visible === 1 ? '' : 's'}${terms.length ? ` matching “${filter.value}”` : ''}`;
    $('[data-filter-empty]').hidden = visible !== 0;
  });
}

const dialog = $('.search-dialog');
const searchInput = $('#docs-search');
const results = $('[data-search-results]');
const status = $('[data-search-status]');
const rootUrl = new URL('../', import.meta.url);
let searchIndex, loading, searchLinks = [], selected = -1, previousFocus;
results.id = 'search-results';
results.setAttribute('role', 'listbox');
results.setAttribute('aria-label', 'Search results');
searchInput.setAttribute('role', 'combobox');
searchInput.setAttribute('aria-controls', results.id);
searchInput.setAttribute('aria-expanded', 'true');
searchInput.setAttribute('aria-autocomplete', 'list');

async function loadIndex() {
  if (searchIndex) return searchIndex;
  if (!loading) loading = fetch(new URL('search.json', import.meta.url))
    .then(response => { if (!response.ok) throw new Error('Search index unavailable'); return response.json(); })
    .then(index => { searchIndex = index; return index; })
    .catch(error => { loading = null; throw error; });
  return loading;
}
function selectResult(index) {
  selected = index;
  searchLinks.forEach((link, i) => link.setAttribute('aria-selected', String(i === index)));
  if (searchLinks[index]) {
    searchInput.setAttribute('aria-activedescendant', searchLinks[index].id);
    searchLinks[index].scrollIntoView({ block: 'nearest' });
  } else searchInput.removeAttribute('aria-activedescendant');
}
function renderSearch() {
  if (!searchIndex) return;
  const terms = searchInput.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const defaults = ['quickstart', 'reference', 'authentication', 'subscriptions', 'types/SessionRow'];
  const found = terms.length ? searchIndex.map(item => {
    const title = item.title.toLowerCase(), id = item.id.toLowerCase();
    const desc = `${item.description} ${item.group}`.toLowerCase();
    const body = item.text.toLowerCase();
    const scores = terms.map(term => title.includes(term) ? 20 : id.includes(term) ? 12 : desc.includes(term) ? 6 : body.includes(term) ? 1 : 0);
    return { item, score: scores.every(Boolean) ? scores.reduce((a, b) => a + b, 0) : 0 };
  }).filter(row => row.score > 0).sort((a, b) => b.score - a.score || a.item.title.localeCompare(b.item.title)).map(row => row.item) : defaults.map(id => searchIndex.find(item => item.id === id)).filter(Boolean);
  results.replaceChildren();
  searchLinks = [];
  if (!found.length) {
    const empty = document.createElement('p');
    empty.className = 'search-empty';
    empty.textContent = `No results for “${searchInput.value}”. Try a route, a type, or “authentication”.`;
    results.append(empty);
  }
  for (const [index, item] of found.slice(0, 30).entries()) {
    const link = document.createElement('a');
    link.href = new URL(item.id ? `${item.id}/` : './', rootUrl).href;
    link.id = `search-result-${index}`;
    link.setAttribute('role', 'option');
    link.setAttribute('aria-selected', 'false');
    const title = document.createElement('strong');
    title.textContent = item.title;
    const group = document.createElement('span');
    group.className = 'search-group';
    group.textContent = item.group;
    const summary = document.createElement('p');
    summary.textContent = item.description;
    link.append(group, title, summary);
    link.addEventListener('pointermove', () => selectResult(index));
    results.append(link);
    searchLinks.push(link);
  }
  status.textContent = terms.length ? `${found.length} result${found.length === 1 ? '' : 's'}${found.length > 30 ? ' · showing first 30' : ''}` : 'Suggested pages';
  selectResult(searchLinks.length ? 0 : -1);
}
async function openSearch() {
  if (document.body.classList.contains('menu-open')) {
    menu(false, false);
    previousFocus = menuButton;
  } else previousFocus = document.activeElement;
  if (!dialog.open) dialog.showModal();
  document.body.classList.add('search-open');
  searchInput.focus();
  searchInput.select();
  status.textContent = 'Loading search…';
  try { await loadIndex(); renderSearch(); }
  catch { status.textContent = 'Search could not load. Close and reopen to retry.'; }
}
$$('[data-search-open]').forEach(button => button.addEventListener('click', openSearch));
$('[data-search-close]').addEventListener('click', () => dialog.close());
dialog.addEventListener('click', event => { if (event.target === dialog) dialog.close(); });
dialog.addEventListener('close', () => { document.body.classList.remove('search-open'); previousFocus?.focus(); });
searchInput.addEventListener('input', renderSearch);
searchInput.addEventListener('keydown', event => {
  if (['ArrowDown', 'ArrowUp'].includes(event.key) && searchLinks.length) {
    event.preventDefault();
    selectResult((selected + (event.key === 'ArrowDown' ? 1 : -1) + searchLinks.length) % searchLinks.length);
  } else if (event.key === 'Enter' && searchLinks[selected]) {
    event.preventDefault(); searchLinks[selected].click();
  }
});
document.addEventListener('keydown', event => {
  const editing = event.target instanceof HTMLElement && (['INPUT', 'TEXTAREA', 'SELECT'].includes(event.target.tagName) || event.target.isContentEditable);
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
    event.preventDefault(); dialog.open ? dialog.close() : openSearch();
  } else if (event.key === '/' && !editing && !dialog.open) {
    event.preventDefault(); filter ? filter.focus() : openSearch();
  }
});
for (const trigger of $$('[data-search-open]')) {
  const shortcut = $('kbd', trigger);
  if (shortcut && !/Mac|iPhone|iPad/.test(navigator.platform)) shortcut.textContent = 'Ctrl K';
}

// The contents rail follows the nearest section without taking over scrolling.
const headings = $$('main h2[id]');
const railLinks = $$('.page-rail nav a');
let scrollPending = false;
function updateRail() {
  scrollPending = false;
  let current = headings[0];
  for (const heading of headings) if (heading.getBoundingClientRect().top <= 150) current = heading;
  for (const link of railLinks) {
    if (link.hash === `#${current?.id}`) link.setAttribute('aria-current', 'location');
    else link.removeAttribute('aria-current');
  }
}
window.addEventListener('scroll', () => {
  if (!scrollPending) { scrollPending = true; requestAnimationFrame(updateRail); }
}, { passive: true });
updateRail();
