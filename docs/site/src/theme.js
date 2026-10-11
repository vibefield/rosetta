// Run before first paint. Storage is optional (private/embedded contexts).
(() => {
  let preference;
  try { preference = localStorage.getItem('rosetta-docs-theme'); } catch {}
  const dark = preference === 'dark' || (preference !== 'light' && matchMedia('(prefers-color-scheme: dark)').matches);
  document.documentElement.classList.toggle('dark', dark);
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
})();
