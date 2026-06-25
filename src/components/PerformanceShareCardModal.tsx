import { useCallback, useEffect, useMemo, useState } from "react";
import {
  buildPerformanceShareOnXUrl,
  createPerformanceShareOverlayUrl,
  PERFORMANCE_SHARE_CARD_DOWNLOAD_MIME,
  PERFORMANCE_SHARE_CARD_DOWNLOAD_QUALITY,
  performanceShareCardBackgroundUrl,
  renderPerformanceShareCardBlob,
  type PerformanceShareCardPayload,
} from "../lib/performance-share-card";

type PerformanceShareCardModalProps = {
  card: PerformanceShareCardPayload;
  backgroundPath: string;
  onClose: () => void;
};

export function PerformanceShareCardModal({
  card,
  backgroundPath,
  onClose,
}: PerformanceShareCardModalProps) {
  const [overlayUrl, setOverlayUrl] = useState<string | null>(null);
  const [copyingImage, setCopyingImage] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const nextOverlayUrl = createPerformanceShareOverlayUrl(card);
    setOverlayUrl(nextOverlayUrl);
    return () => URL.revokeObjectURL(nextOverlayUrl);
  }, [card]);

  useEffect(() => {
    if (!notice) {
      return undefined;
    }
    const timeout = window.setTimeout(() => setNotice(null), 2500);
    return () => window.clearTimeout(timeout);
  }, [notice]);

  const fileName = useMemo(() => `EVPLUS-${card.filenameSlug}-share-card.jpg`, [card.filenameSlug]);

  const handleCopyImage = useCallback(async () => {
    if (copyingImage) {
      return;
    }
    setCopyingImage(true);
    try {
      const blob = await renderPerformanceShareCardBlob(card, backgroundPath);
      if (!("ClipboardItem" in window)) {
        throw new Error("Image clipboard is not available in this browser.");
      }
      await navigator.clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
      setNotice("Image copied.");
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not copy image.");
    } finally {
      setCopyingImage(false);
    }
  }, [backgroundPath, card, copyingImage]);

  const handleDownload = useCallback(async () => {
    if (downloading) {
      return;
    }
    setDownloading(true);
    try {
      const blob = await renderPerformanceShareCardBlob(card, backgroundPath, null, {
        mimeType: PERFORMANCE_SHARE_CARD_DOWNLOAD_MIME,
        quality: PERFORMANCE_SHARE_CARD_DOWNLOAD_QUALITY,
      });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = fileName;
      document.body.append(anchor);
      anchor.click();
      anchor.remove();
      URL.revokeObjectURL(url);
      setNotice("Download started.");
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not download image.");
    } finally {
      setDownloading(false);
    }
  }, [backgroundPath, card, downloading, fileName]);

  const handleShareOnX = useCallback(() => {
    window.open(buildPerformanceShareOnXUrl(card), "_blank", "noopener,noreferrer");
  }, [card]);

  return (
    <div className="checkout-modal performance-share-modal" role="dialog" aria-modal="true" aria-label="Share performance card">
      <button type="button" className="checkout-modal__backdrop" aria-label="Close share card" onClick={onClose} />
      <div className="checkout-modal__card performance-share-modal__card">
        <div className="checkout-modal__header performance-share-modal__header">
          <button type="button" className="checkout-modal__close" onClick={onClose} aria-label="Close">
            X
          </button>
          <div className="checkout-modal__title">Share {card.title}</div>
          <div className="checkout-modal__subtitle">
            Preview uses the selected character and current performance data.
          </div>
        </div>

        <div className="performance-share-modal__preview">
          <img
            className="performance-share-modal__preview-background"
            src={performanceShareCardBackgroundUrl(backgroundPath)}
            alt=""
            decoding="async"
          />
          {overlayUrl ? (
            <img
              className="performance-share-modal__preview-overlay"
              src={overlayUrl}
              alt={`${card.title} share card preview`}
              decoding="async"
            />
          ) : null}
        </div>

        {error ? <div className="referral-message referral-message--error">{error}</div> : null}
        {notice ? <div className="referral-message referral-message--ok">{notice}</div> : null}

        <div className="evpoint-studio__tools performance-share-modal__tools">
          <button type="button" className="evpoint-tool" onClick={() => void handleCopyImage()} disabled={copyingImage}>
            <span className="evpoint-tool__icon" aria-hidden="true">
              <svg viewBox="0 0 24 24" fill="none">
                <rect x="8" y="8" width="10" height="10" rx="2" stroke="currentColor" strokeWidth="2" />
                <path d="M6 16H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
              </svg>
            </span>
            <span className="evpoint-tool__label">{copyingImage ? "Copying..." : "Copy image"}</span>
          </button>

          <button type="button" className="evpoint-tool" onClick={() => void handleDownload()} disabled={downloading}>
            <span className="evpoint-tool__icon" aria-hidden="true">
              <svg viewBox="0 0 24 24" fill="none">
                <path d="M12 4v10m0 0 4-4m-4 4-4-4" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
                <path d="M5 20h14" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
              </svg>
            </span>
            <span className="evpoint-tool__label">{downloading ? "Downloading..." : "Download"}</span>
          </button>

          <button type="button" className="evpoint-tool evpoint-tool--primary" onClick={handleShareOnX}>
            <span className="evpoint-tool__icon" aria-hidden="true">
              <svg viewBox="0 0 24 24" fill="none">
                <path d="M4 5.5 10.8 13 4.4 20h2.4l5.1-5.6L17 20h3L13.3 12.6 19.4 5.5H17l-4.8 5.3-4.8-5.3H4Z" fill="currentColor" />
              </svg>
            </span>
            <span className="evpoint-tool__label">Share on X</span>
          </button>
        </div>
      </div>
    </div>
  );
}
