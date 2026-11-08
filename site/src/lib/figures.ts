// The headline codegen/box figures, as shields.io endpoint JSON (scripts/conformance-badge.sh).
// The README badges and this page read the same files from the badge gist, which holds the last
// figures published there. Normal releases now package these files as release assets
// (scripts/package-release-badges.sh); both readers move to those assets once the first normal
// release carries them. The page reads the JSON at build time and again in the browser.

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
