// serve.ts: the demo page, and every run under build/runs.
//   bun demo/serve.ts        then open http://localhost:8813
const here = import.meta.dir;
const runs = `${here}/build/runs`;
const port = +(process.env.PORT || 8813);
const types: Record<string, string> = { png: "image/png", avif: "image/avif", json: "application/json" };

Bun.serve({
  port,
  async fetch(req) {
    const p = decodeURIComponent(new URL(req.url).pathname);
    if (p.includes("..")) return new Response("bad path", { status: 400 });
    if (p === "/") return new Response(Bun.file(`${here}/page.html`));
    if (p === "/runs.json") {
      const list: unknown[] = [];
      for (const name of new Bun.Glob("*/report.json").scanSync(runs)) {
        list.push(JSON.parse(await Bun.file(`${runs}/${name}`).text()));
      }
      list.sort((a: any, b: any) => b.date.localeCompare(a.date));
      return Response.json(list);
    }
    if (p.startsWith("/runs/")) {
      const f = Bun.file(`${runs}/${p.slice(6)}`);
      if (await f.exists()) {
        const ext = p.split(".").pop()!;
        return new Response(f, { headers: { "content-type": types[ext] ?? "application/octet-stream" } });
      }
    }
    return new Response("not found", { status: 404 });
  },
});
console.log(`AV2 demo on http://localhost:${port}`);
