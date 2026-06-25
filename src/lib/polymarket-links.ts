const POLYMARKET_HOST_RE = /(^|\.)polymarket\.com$/i;
const SLUG_RE = /^[a-z0-9][a-z0-9-]*$/i;

function readSlugParts(value: string): string[] | null {
  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }

  try {
    const url = new URL(trimmed);
    if (!POLYMARKET_HOST_RE.test(url.hostname)) {
      return null;
    }
    const parts = url.pathname.split("/").map((part) => part.trim()).filter(Boolean);
    const eventIndex = parts.findIndex((part) => part.toLowerCase() === "event");
    if (eventIndex >= 0) {
      const eventPath = parts.slice(eventIndex + 1);
      return eventPath.length > 0 ? eventPath : null;
    }
    const slug = parts[parts.length - 1];
    return slug ? [slug] : null;
  } catch {
    const withoutQuery = trimmed.split(/[?#]/, 1)[0]?.replace(/^\/+/, "") ?? "";
    const parts = withoutQuery.split("/").map((part) => part.trim()).filter(Boolean);
    if (parts.some((part) => part === "." || part === "..")) {
      return null;
    }
    const eventIndex = parts.findIndex((part) => part.toLowerCase() === "event");
    if (eventIndex >= 0) {
      const eventPath = parts.slice(eventIndex + 1);
      return eventPath.length > 0 ? eventPath : null;
    }
    const slug = parts[parts.length - 1];
    return slug ? [slug] : null;
  }
}

export function normalizePolymarketSlug(value: string | null | undefined): string | null {
  if (!value) {
    return null;
  }
  const parts = readSlugParts(value);
  if (!parts || parts.some((part) => !SLUG_RE.test(part))) {
    return null;
  }
  const segment = parts[parts.length - 1];
  if (!segment || !SLUG_RE.test(segment)) {
    return null;
  }
  return segment;
}

export function buildPolymarketMarketUrl(
  marketSlug: string | null | undefined,
  eventSlug?: string | null,
): string | null {
  const marketParts = marketSlug ? readSlugParts(marketSlug) : null;
  const safeMarketParts = marketParts && marketParts.every((part) => SLUG_RE.test(part)) ? marketParts : null;
  const eventParts = eventSlug ? readSlugParts(eventSlug) : null;
  const safeEventParts = eventParts && eventParts.every((part) => SLUG_RE.test(part)) ? eventParts : null;
  const market = safeMarketParts?.[safeMarketParts.length - 1] ?? null;
  const event =
    safeEventParts?.[0] ??
    (safeMarketParts && safeMarketParts.length > 1 ? safeMarketParts[0] : null);
  if (event && market && event !== market) {
    return `https://polymarket.com/event/${event}/${market}`;
  }
  const slug = market ?? event;
  return slug ? `https://polymarket.com/event/${slug}` : null;
}
