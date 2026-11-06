// The headline conformance figures. The master build writes them as shields.io endpoint JSON to a
// gist (see the `release` job in .github/workflows/ci.yml and scripts/conformance-badge.sh); the
// page reads the same JSON at build time and again in the browser, so it never shows numbers the
// badges do not.

export const gist = "https://gist.githubusercontent.com/qnox/dec8149bc4f43b203d6cc9adc14f2026/raw/";

export interface Figure {
  pct: number;
  count: number;
  of: number;
}

// conformance-badge.sh writes "<pct>% (<count>/<of>)" or "<pct>% (<count>/<of> bytes)".
export function parseMessage(message: string): Figure | null {
  const match = /^(\d+(?:\.\d+)?)% \((\d+)\/(\d+)(?: bytes)?\)$/.exec(message);
  if (!match) return null;
  return { pct: Number(match[1]), count: Number(match[2]), of: Number(match[3]) };
}

export async function readFigure(file: string): Promise<Figure | null> {
  try {
    const response = await fetch(gist + file, { signal: AbortSignal.timeout(8000) });
    if (!response.ok) return null;
    const body = await response.json();
    return typeof body?.message === "string" ? parseMessage(body.message) : null;
  } catch {
    return null;
  }
}
