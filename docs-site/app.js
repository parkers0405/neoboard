async function renderGuide() {
  const link = [...document.querySelectorAll('.guides a')].find(a => a.pathname === location.pathname);
  link.setAttribute('aria-current', 'page');
  const title = link.textContent;
  document.title = `${title} · NeoBoard guides`;
  const source = location.pathname.replace('/docs/', '/source/');
  const response = await fetch(source, { credentials: 'omit' });
  if (!response.ok) throw new Error(`Cannot read docs/${source.split('/').pop()} (HTTP ${response.status}). Save the Markdown file and refresh.`);
  const description = await response.text();
  Scalar.createApiReference('#reference', {
    content: { openapi: '3.1.0', info: { title, version: '', description }, paths: {} },
    withDefaultFonts: false,
    hiddenClients: true,
    hideClientButton: true,
    hideTestRequestButton: true,
    persistAuth: false,
    agent: { disabled: true },
    mcp: { disabled: true },
    telemetry: false,
    showDeveloperTools: 'never',
    documentDownloadType: 'none',
    onLoaded: () => document.querySelector('#status').remove(),
  });
}

renderGuide().catch(error => { document.querySelector('#status').textContent = error.message; });