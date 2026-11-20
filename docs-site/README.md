# Preview the NeoBoard guides

From the repository root, run `python3 docs-site/preview.py` (Python 3.9+), then open <http://127.0.0.1:8765/>. Use `--port 8766` if the port is busy. Stop with Ctrl-C.

The first run downloads the standalone Scalar API Reference 1.77.0 bundle used by the Neoism docs reference and verifies its SHA-256 before serving it. Later runs use the verified `docs-site/.cache/` copy offline. The preview binds only to IPv4 loopback; there is no deployment step or NeoBoard backend connection.

Each guide reads its corresponding `docs/{start,boards,tasks,agents,operations}.md` file on request. Save and refresh to see changes; relative links such as `[Boards](boards.md)` open the rendered guide. Missing files show an error rather than placeholder content. Scalar receives Markdown descriptions with no API operations, authentication schemes or interactive requests.