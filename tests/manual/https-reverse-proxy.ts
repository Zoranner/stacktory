const [backendUrlArgument, portArgument, certPathArgument, keyPathArgument] =
  process.argv.slice(2);
const backendUrl = new URL(
  backendUrlArgument ?? "http://127.0.0.1:18761",
);
const port = Number(portArgument ?? "18762");
const certPath = certPathArgument;
const keyPath = keyPathArgument;

if (!keyPath || !certPath) {
  throw new Error("STACKTORY_TLS_KEY and STACKTORY_TLS_CERT are required");
}

Bun.serve({
  port,
  tls: {
    key: Bun.file(keyPath),
    cert: Bun.file(certPath),
  },
  async fetch(request) {
    const source = new URL(request.url);
    const upstream = new URL(`${source.pathname}${source.search}`, backendUrl);
    const headers = new Headers(request.headers);
    headers.delete("host");
    headers.delete("accept-encoding");
    const body =
      request.method === "GET" || request.method === "HEAD"
        ? undefined
        : request.body;
    const response = await fetch(upstream, {
      method: request.method,
      headers,
      body,
    });
    return new Response(response.body, {
      status: response.status,
      headers: response.headers,
    });
  },
});
