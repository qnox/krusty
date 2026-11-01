// The metrics page's data, published beside it for anyone who wants the numbers rather than the page.
import { collectMetrics } from "../../scripts/metrics.mjs";

export function GET() {
  return new Response(`${JSON.stringify(collectMetrics(process.env.KRUSTY_METRICS_INPUT), null, 1)}\n`, {
    headers: { "Content-Type": "application/json" },
  });
}
