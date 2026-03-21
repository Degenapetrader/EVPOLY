function badgeLetters(symbol?: string | null, title?: string | null): string {
  const normalizedSymbol = symbol?.trim().toUpperCase();
  if (normalizedSymbol) {
    return normalizedSymbol.length <= 4 ? normalizedSymbol : normalizedSymbol.slice(0, 3);
  }
  const source = title?.trim() ?? "";
  if (!source) return "EV";
  const letters = source
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((word) => word[0]?.toUpperCase() ?? "")
    .join("");
  return letters || "EV";
}

export function MarketBadge({
  title,
  symbol,
  imageUrl,
  iconUrl,
  size = "md",
}: {
  title?: string | null;
  symbol?: string | null;
  imageUrl?: string | null;
  iconUrl?: string | null;
  size?: "sm" | "md" | "lg";
}) {
  const media = imageUrl || iconUrl;
  const letters = badgeLetters(symbol, title);

  return (
    <div className={`market-badge market-badge--${size}`}>
      {media ? (
        <img
          src={media}
          alt={title || symbol || "Market"}
          className="market-badge__image"
          loading="lazy"
        />
      ) : (
        <span className="market-badge__symbol mono-data">{letters}</span>
      )}
    </div>
  );
}
