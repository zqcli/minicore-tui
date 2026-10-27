// Runs before the first paint, so the page never flashes the wrong theme or text size; the app calls it again when a setting changes.
function applyTheme() {
  const { theme, text = 1 } = JSON.parse(localStorage.getItem('minicore.prefs') || '{}');
  const dark = matchMedia('(prefers-color-scheme: dark)').matches;
  const pick = osDark => theme === 'light' || theme === 'dark' ? theme : osDark ? 'dark' : 'light';
  const root = document.documentElement, bar = { light: '#ffffff', dark: '#0e0e10' };
  root.dataset.theme = pick(dark);
  root.style.setProperty('--ts', text);
  document.getElementById('tc').content = bar[pick(false)];
  document.getElementById('tcd').content = bar[pick(true)];
}
applyTheme();
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', applyTheme);
