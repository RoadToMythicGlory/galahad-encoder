import { useEffect, useRef, useState } from "react";
import Hls from "hls.js";
import { api } from "./api";
import type { AudioLevels, PreviewStatus } from "./types";

/** Perceptual mapping of a 0..1 linear level to a 0..100 bar width. */
function barWidth(level: number): number {
  return Math.min(100, Math.sqrt(Math.max(0, level)) * 100);
}

/** Format a 0..1 linear level as dBFS for the section header. */
function toDb(level: number): string {
  if (level <= 0.0001) return "-\u221e";
  return `${(20 * Math.log10(level)).toFixed(0)} dB`;
}

function Meter({ label, level }: { label: string; level: number }) {
  return (
    <div className="pa-row">
      <span className="pa-label" title={label}>
        {label}
      </span>
      <div className="meter">
        <div
          className="meter-fill"
          style={{ clipPath: `inset(0 ${100 - barWidth(level)}% 0 0)` }}
        />
      </div>
    </div>
  );
}

export function PreviewWindow() {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const hlsRef = useRef<Hls | null>(null);
  const [status, setStatus] = useState<PreviewStatus | null>(null);
  const [levels, setLevels] = useState<AudioLevels | null>(null);
  const [showProgram, setShowProgram] = useState(true);
  const [showChannels, setShowChannels] = useState(true);
  const [playerError, setPlayerError] = useState<string | null>(null);
  // Bumped to force a fresh HLS attach after a fatal player error.
  const [reloadTick, setReloadTick] = useState(0);

  // Poll preview availability from the backend.
  useEffect(() => {
    let active = true;
    const poll = async () => {
      try {
        const s = await api.getPreviewStatus();
        if (active) setStatus(s);
      } catch {
        /* transient */
      }
    };
    void poll();
    const timer = setInterval(() => void poll(), 1500);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, []);

  // Poll audio levels frequently for a responsive meter.
  useEffect(() => {
    let active = true;
    const poll = async () => {
      try {
        const l = await api.getAudioLevels();
        if (active) setLevels(l);
      } catch {
        /* transient */
      }
    };
    void poll();
    const timer = setInterval(() => void poll(), 100);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, []);

  // Attach / re-attach the HLS stream whenever the feed becomes available.
  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    const url = status?.available ? status.url : null;
    if (!url) return;

    setPlayerError(null);
    let hls: Hls | null = null;
    let retry: number | undefined;

    if (Hls.isSupported()) {
      hls = new Hls({
        lowLatencyMode: true,
        liveSyncDurationCount: 3,
        backBufferLength: 10,
      });
      hlsRef.current = hls;
      hls.loadSource(url);
      hls.attachMedia(video);
      hls.on(Hls.Events.MANIFEST_PARSED, () => {
        void video.play().catch(() => {});
      });
      hls.on(Hls.Events.ERROR, (_evt, data) => {
        // Non-fatal errors self-recover. Fatal errors usually mean segments
        // aren't written yet (just after start) or the session restarted;
        // retry the whole attach shortly.
        if (!data.fatal) return;
        setPlayerError("Buffering the encoded feed…");
        retry = window.setTimeout(() => setReloadTick((t) => t + 1), 1500);
      });
    } else if (video.canPlayType("application/vnd.apple.mpegurl")) {
      video.src = url;
      void video.play().catch(() => {});
    } else {
      setPlayerError("This webview cannot play HLS.");
    }

    return () => {
      if (retry) window.clearTimeout(retry);
      if (hls) hls.destroy();
      hlsRef.current = null;
      video.removeAttribute("src");
      try {
        video.load();
      } catch {
        /* ignore */
      }
    };
  }, [status?.available, status?.url, reloadTick]);

  const available = !!status?.available;
  const live = available && !playerError;
  const overlay = !available
    ? status?.reason ?? "Waiting for the encoded output…"
    : playerError;

  return (
    <div className="preview-app">
      <header className="preview-bar">
        <span className="brand">Encoder Preview</span>
        <span className={`preview-tag ${live ? "live" : "idle"}`}>
          {live ? "LIVE" : "WAITING"}
        </span>
      </header>
      <div className="preview-stage">
        <video
          ref={videoRef}
          className="preview-video"
          controls
          autoPlay
          muted
          playsInline
        />
        {overlay ? (
          <div className="preview-overlay">
            <div className="preview-spinner" aria-hidden />
            <p>{overlay}</p>
          </div>
        ) : null}
      </div>

      <div className={`preview-audio${levels?.active ? "" : " is-idle"}`}>
        <section className="pa-group">
          <button
            className="pa-head"
            onClick={() => setShowProgram((v) => !v)}
            aria-expanded={showProgram}
          >
            <span className={`pa-caret${showProgram ? " open" : ""}`} aria-hidden>
              &#9656;
            </span>
            <span className="pa-title">Program</span>
            <span className="pa-meta">
              {toDb(Math.max(levels?.programPeakL ?? 0, levels?.programPeakR ?? 0))}
            </span>
          </button>
          {showProgram ? (
            <div className="pa-body">
              <Meter label="L" level={levels?.programPeakL ?? 0} />
              <Meter label="R" level={levels?.programPeakR ?? 0} />
            </div>
          ) : null}
        </section>

        <section className="pa-group">
          <button
            className="pa-head"
            onClick={() => setShowChannels((v) => !v)}
            aria-expanded={showChannels}
          >
            <span className={`pa-caret${showChannels ? " open" : ""}`} aria-hidden>
              &#9656;
            </span>
            <span className="pa-title">Channels</span>
            <span className="pa-meta">{levels?.channels.length ?? 0}</span>
          </button>
          {showChannels ? (
            <div className="pa-body">
              {levels && levels.channels.length > 0 ? (
                levels.channels.map((c, i) => (
                  <Meter key={`${c.label}-${i}`} label={c.label} level={c.peak} />
                ))
              ) : (
                <p className="hint">No audio channels in this stream.</p>
              )}
            </div>
          ) : null}
        </section>
      </div>

      <footer className="preview-foot">
        <span className="hint">
          Live monitor of the encoded SRT output. Expect a few seconds of latency.
        </span>
      </footer>
    </div>
  );
}
