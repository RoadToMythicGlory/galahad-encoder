import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "./api";
import { Button, Card, Field, Pill, Toggle } from "./ui";
import {
  PRESETS,
  type AudioSourceKind,
  type Capabilities,
  type ClientConfig,
  type Identity,
  type ProcessInfo,
  type SrtMode,
  type StreamStatus,
  type TransportKind,
  type VideoDeviceInfo,
  type WindowInfo,
} from "./types";

const LATENCY_OPTIONS = [
  { value: 120, label: "Low (120 ms)" },
  { value: 200, label: "Normal (200 ms)" },
  { value: 400, label: "High / unstable network (400 ms)" },
];

const IPV4_RE = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/;

function isValidIpv4(value: string): boolean {
  const m = IPV4_RE.exec(value.trim());
  if (!m) return false;
  return m.slice(1).every((o) => Number(o) <= 255);
}

function formatBandwidth(mbps: number): string {
  return mbps >= 1000 ? `${(mbps / 1000).toFixed(2)} Gbit/s` : `${mbps} Mbit/s`;
}

const AUDIO_KIND_LABEL: Record<AudioSourceKind, string> = {
  system: "Desktop audio",
  microphone: "Microphone",
  application: "Application",
};

const STATE_TONE: Record<string, string> = {
  idle: "muted",
  starting: "info",
  streaming: "ok",
  degraded: "warn",
  reconnecting: "warn",
  failed: "bad",
};

export function App() {
  const [config, setConfig] = useState<ClientConfig | null>(null);
  const [caps, setCaps] = useState<Capabilities | null>(null);
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [windows, setWindows] = useState<WindowInfo[]>([]);
  const [videoDevices, setVideoDevices] = useState<VideoDeviceInfo[]>([]);
  const [audioProcesses, setAudioProcesses] = useState<ProcessInfo[]>([]);
  const [selectedWindowId, setSelectedWindowId] = useState<number | null>(null);
  const [status, setStatus] = useState<StreamStatus | null>(null);
  const [logs, setLogs] = useState<string[]>([]);
  const [message, setMessage] = useState<string | null>(null);
  const logRef = useRef<HTMLPreElement | null>(null);

  // Initial load.
  useEffect(() => {
    (async () => {
      try {
        const [cfg, capabilities, id, wins, devices, procs] = await Promise.all([
          api.getConfig(),
          api.getCapabilities(),
          api.getIdentity(),
          api.listWindows(),
          api.listVideoDevices(),
          api.listAudioProcesses(),
        ]);
        setConfig(cfg);
        setCaps(capabilities);
        setIdentity(id);
        setWindows(wins);
        setVideoDevices(devices);
        setAudioProcesses(procs);
      } catch (e) {
        setMessage(`Startup failed: ${String(e)}`);
      }
    })();
  }, []);

  // Status + log polling.
  useEffect(() => {
    const timer = setInterval(async () => {
      try {
        const [s, l] = await Promise.all([api.getStatus(), api.getLogs()]);
        setStatus(s);
        setLogs(l);
      } catch {
        /* transient */
      }
    }, 1000);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    if (logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [logs]);

  const updateConfig = useCallback(
    (mutator: (draft: ClientConfig) => void) => {
      setConfig((prev) => {
        if (!prev) return prev;
        const next = structuredClone(prev) as ClientConfig;
        mutator(next);
        api.saveConfig(next).catch((e) => setMessage(`Save failed: ${String(e)}`));
        return next;
      });
    },
    []
  );

  const canEncode = useMemo(() => {
    if (!caps || !config) return false;
    if (caps.encoders.length === 0) return false;
    const hasHardware = caps.encoderBackends.some((b) => b.hardware);
    return hasHardware || config.encoder.allowSoftware;
  }, [caps, config]);

  const isListener = (config?.srtMode ?? "caller") === "listener";

  const maxCallers = Math.min(3, Math.max(1, config?.srtMaxCallers ?? 1));

  // In listener mode each caller connects to its own consecutive port.
  const listenerEndpoints = useMemo(() => {
    if (!config) return [] as string[];
    const host = config.destinationHost.trim() || "<your-ip>";
    const basePort = config.destinationPort;
    if (!basePort) return [];
    return Array.from({ length: maxCallers }, (_, i) => `${host}:${basePort + i}`);
  }, [config, maxCallers]);

  const destinationValid = useMemo(() => {
    if (!config) return false;
    const portOk = config.destinationPort > 0 && config.destinationPort <= 65535;
    // In listener mode the host is advisory (we bind all interfaces), so only
    // the port is required. In caller mode we need a destination host too.
    if (config.srtMode === "listener") return portOk;
    return config.destinationHost.trim().length > 0 && portOk;
  }, [config]);

  const hevcAvailable = useMemo(
    () => caps?.encoderBackends.some((b) => b.codec === "hevc") ?? false,
    [caps]
  );

  const transport: TransportKind = config?.transport ?? "srt";
  const isSt2110 = transport === "st2110";

  const activeProfile = useMemo(
    () => PRESETS.find((p) => p.id === config?.presetId) ?? null,
    [config?.presetId]
  );

  const st2110Valid = useMemo(() => {
    if (!config) return false;
    const st = config.st2110;
    if (!isValidIpv4(st.videoDestIp) || st.videoDestPort <= 0) return false;
    if (st.audioEnabled && (!isValidIpv4(st.audioDestIp) || st.audioDestPort <= 0)) {
      return false;
    }
    if (st.interfaceIp.trim().length > 0 && !isValidIpv4(st.interfaceIp)) {
      return false;
    }
    if (st.payloadType < 96 || st.payloadType > 127) return false;
    return true;
  }, [config]);

  const deviceSelected = useMemo(
    () => (config?.videoSource.deviceName ?? "").trim().length > 0,
    [config?.videoSource.deviceName]
  );

  const isStreaming = status
    ? ["starting", "streaming", "degraded", "reconnecting"].includes(status.state)
    : false;

  const refreshDevices = useCallback(async () => {
    setVideoDevices(await api.listVideoDevices());
  }, []);

  const selectWindow = useCallback(
    async (win: WindowInfo) => {
      setSelectedWindowId(win.id);
      const label = `${win.processName} — ${win.title}`;
      await api.selectWindow(win.id, label);
      updateConfig((c) => {
        c.sourceWindowTitle = win.title;
        c.sourceProcessName = win.processName;
      });
    },
    [updateConfig]
  );

  const selectDevice = useCallback(
    (device: VideoDeviceInfo) => {
      updateConfig((c) => {
        c.videoSource.type = "device";
        c.videoSource.deviceName = device.id;
      });
    },
    [updateConfig]
  );

  const setTransport = useCallback(
    (next: TransportKind) => {
      updateConfig((c) => {
        c.transport = next;
        // ST 2110 can only originate from a physical capture device; SRT keeps
        // whatever source (camera or window) the user already picked.
        if (next === "st2110") {
          c.videoSource.type = "device";
        }
      });
    },
    [updateConfig]
  );

  const toggleSourceMode = useCallback(() => {
    updateConfig((c) => {
      c.videoSource.type = c.videoSource.type === "device" ? "window" : "device";
    });
  }, [updateConfig]);

  const refreshWindows = useCallback(async () => {
    setWindows(await api.listWindows());
  }, []);

  const refreshAudioProcesses = useCallback(async () => {
    setAudioProcesses(await api.listAudioProcesses());
  }, []);

  const addAudioSource = useCallback(
    (kind: AudioSourceKind) => {
      updateConfig((c) => {
        const id = `${kind}-${Date.now().toString(36)}`;
        c.audio.sources.push({
          id,
          type: kind,
          enabled: true,
          muted: false,
          gain: 1,
          deviceId: null,
          processId: null,
          label: AUDIO_KIND_LABEL[kind],
        });
      });
    },
    [updateConfig]
  );

  const removeAudioSource = useCallback(
    (id: string) => {
      updateConfig((c) => {
        c.audio.sources = c.audio.sources.filter((s) => s.id !== id);
      });
    },
    [updateConfig]
  );

  const updateAudioSource = useCallback(
    (id: string, mutator: (s: ClientConfig["audio"]["sources"][number]) => void) => {
      updateConfig((c) => {
        const s = c.audio.sources.find((x) => x.id === id);
        if (s) mutator(s);
      });
    },
    [updateConfig]
  );

  const [detectingIp, setDetectingIp] = useState(false);
  const [ipInfo, setIpInfo] = useState<{ public: string | null; local: string | null } | null>(
    null
  );

  // Fill the IP box with the public/WAN address by default: listener mode is
  // almost always used to receive a caller coming in over the internet, so the
  // WAN IP is the one the operator needs to hand out. The LAN address stays
  // available as a chip below for same-network callers.
  const detectAndFillIp = useCallback(async () => {
    setDetectingIp(true);
    try {
      const info = await api.detectIp();
      setIpInfo(info);
      const ip = info.public ?? info.local;
      if (ip) {
        updateConfig((c) => (c.destinationHost = ip));
      } else {
        setMessage("Could not detect an IP address. Enter it manually.");
      }
    } catch (e) {
      setMessage(`IP detection failed: ${String(e)}`);
    } finally {
      setDetectingIp(false);
    }
  }, [updateConfig]);

  const setSrtMode = useCallback(
    (mode: SrtMode) => {
      updateConfig((c) => (c.srtMode = mode));
      // Entering listener mode: auto-fill our address to share with the caller.
      if (mode === "listener") {
        void detectAndFillIp();
      }
    },
    [detectAndFillIp, updateConfig]
  );

  // Pre-fill the shareable IP once if we load straight into listener mode.
  const autoIpDone = useRef(false);
  useEffect(() => {
    if (autoIpDone.current || !config) return;
    if (config.srtMode === "listener" && config.destinationHost.trim() === "") {
      autoIpDone.current = true;
      void detectAndFillIp();
    }
  }, [config, detectAndFillIp]);

  useEffect(() => {
    if (!config || !caps || selectedWindowId !== null || windows.length === 0) {
      return;
    }
    if (caps.capture === "browser-preview" || caps.capture === "none") {
      return;
    }

    const savedWindow = windows.find((win) => {
      const processMatches =
        !config.sourceProcessName || win.processName === config.sourceProcessName;
      const titleMatches =
        !config.sourceWindowTitle || win.title === config.sourceWindowTitle;
      return processMatches && titleMatches;
    });

    if (savedWindow && (config.sourceProcessName || config.sourceWindowTitle)) {
      void selectWindow(savedWindow);
    }
  }, [caps, config, selectedWindowId, selectWindow, windows]);

  const onStart = useCallback(async () => {
    try {
      setMessage(null);
      await api.startStream();
    } catch (e) {
      setMessage(`Start failed: ${String(e)}`);
    }
  }, []);

  const onStop = useCallback(async () => {
    try {
      await api.stopStream();
    } catch (e) {
      setMessage(`Stop failed: ${String(e)}`);
    }
  }, []);

  const onPreset = useCallback(
    async (presetId: string) => {
      updateConfig((c) => {
        c.presetId = presetId;
      });
      if (isStreaming) {
        try {
          await api.switchQuality(presetId);
        } catch (e) {
          setMessage(`Switch quality failed: ${String(e)}`);
        }
      }
    },
    [isStreaming, updateConfig]
  );

  if (!config || !caps || !identity) {
    return (
      <div className="app loading">
        <img src="/logo.png" alt="Galahad Client Agent" className="brand-logo" />
        <p>{message ?? "Detecting capabilities…"}</p>
      </div>
    );
  }

  const captureUnavailable = caps.capture === "browser-preview" || caps.capture === "none";
  const previewBlocker = caps.capture === "browser-preview"
    ? "Open the Tauri desktop app to capture and stream."
    : caps.capture === "none"
    ? "Desktop capture is unavailable on this system."
    : null;

  // ST 2110 always captures a device; SRT can capture a camera or a window.
  const sourceMode = isSt2110 ? "device" : config.videoSource.type;
  const sourceBlocker =
    sourceMode === "window"
      ? selectedWindowId === null
        ? "Select a window to capture."
        : null
      : videoDevices.length === 0
      ? "No capture device detected. Connect a camera or capture card, then Refresh."
      : !deviceSelected
      ? "Select a capture device."
      : null;

  const startBlocker = isSt2110
    ? previewBlocker && caps.capture === "browser-preview"
      ? previewBlocker
      : !caps.gstreamerAvailable
      ? "GStreamer not found. Install it to enable ST 2110 output."
      : sourceBlocker
      ? sourceBlocker
      : !st2110Valid
      ? "Check the ST 2110 destination addresses and payload type."
      : null
    : captureUnavailable
    ? previewBlocker
    : !destinationValid
    ? "Enter destination IP and port."
    : sourceBlocker
    ? sourceBlocker
    : !canEncode
    ? "No usable encoder."
    : !caps.ffmpegAvailable
    ? "FFmpeg is missing."
    : null;
  const startDisabled = startBlocker !== null;

  return (
    <div className="app">
      <header className="app-bar">
        <img src="/logo.png" alt="Galahad Client Agent" className="brand-logo" />
        <div className="app-bar-right">
          <Pill tone={STATE_TONE[status?.state ?? "idle"] ?? "muted"}>
            {(status?.state ?? "idle").toUpperCase()}
          </Pill>
        </div>
      </header>

      {message ? <div className="banner">{message}</div> : null}

      <main className="grid">
        {/* Transport */}
        <Card
          title="Transport"
          subtitle="Compressed SRT uplink, or uncompressed SMPTE ST 2110 broadcast IP."
          accent
        >
          <div className="preset-row">
            <button
              className={`preset${transport === "srt" ? " selected" : ""}`}
              onClick={() => setTransport("srt")}
            >
              SRT (compressed)
            </button>
            <button
              className={`preset${transport === "st2110" ? " selected" : ""}`}
              onClick={() => setTransport("st2110")}
            >
              ST 2110 (broadcast IP)
            </button>
          </div>
          {isSt2110 ? (
            <p className="hint">
              ST 2110 sends uncompressed RTP over the media network and needs
              PTP timing plus a high-bandwidth NIC.{" "}
              {caps.gstreamerAvailable
                ? "GStreamer detected."
                : "GStreamer was not found on this PC."}
            </p>
          ) : (
            <p className="hint">
              SRT connects to your operator's matrix as a caller.
            </p>
          )}
        </Card>

        {/* Destination (SRT) */}
        {!isSt2110 ? (
          <Card
            title="Destination"
            subtitle={
              isListener
                ? "You listen; the caller connects to you. Share your IP and port."
                : "Your operator gives you an IP and port. That's all you need."
            }
          >
            <div className="preset-row">
              <button
                className={`preset${!isListener ? " selected" : ""}`}
                onClick={() => setSrtMode("caller")}
              >
                Caller (connect out)
              </button>
              <button
                className={`preset${isListener ? " selected" : ""}`}
                onClick={() => setSrtMode("listener")}
              >
                Listener (receive)
              </button>
            </div>
            <div className="row">
              <Field label={isListener ? "Your IP (send to caller)" : "Server IP"}>
                <div className="row" style={{ gap: 8 }}>
                  <input
                    value={config.destinationHost}
                    placeholder={isListener ? "auto-detected" : "1.2.3.4"}
                    onChange={(e) =>
                      updateConfig((c) => (c.destinationHost = e.target.value))
                    }
                  />
                  {isListener ? (
                    <Button
                      variant="ghost"
                      onClick={() =>
                        navigator.clipboard.writeText(
                          `${config.destinationHost}:${config.destinationPort}`
                        )
                      }
                    >
                      Copy
                    </Button>
                  ) : null}
                </div>
              </Field>
              <Field label={isListener ? "Listen port" : "Port"}>
                <input
                  value={config.destinationPort || ""}
                  placeholder="9003"
                  inputMode="numeric"
                  onChange={(e) =>
                    updateConfig(
                      (c) =>
                        (c.destinationPort = Number(e.target.value.replace(/\D/g, "")) || 0)
                    )
                  }
                />
              </Field>
            </div>
            {isListener ? (
              <>
                <Field label="Max devices (callers at once)">
                  <div className="preset-row">
                    {[1, 2, 3].map((n) => (
                      <button
                        key={n}
                        className={`preset${maxCallers === n ? " selected" : ""}`}
                        onClick={() => updateConfig((c) => (c.srtMaxCallers = n))}
                      >
                        {n}
                      </button>
                    ))}
                  </div>
                </Field>
                <div className="source-head">
                  <span className="hint">
                    {maxCallers > 1
                      ? `Up to ${maxCallers} callers, each on its own port from ${
                          config.destinationPort || "<port>"
                        }.`
                      : "One caller connects to your address."}{" "}
                    Forward/open these ports for callers outside your LAN.
                  </span>
                  <Button variant="ghost" onClick={detectAndFillIp} disabled={detectingIp}>
                    {detectingIp ? "Detecting…" : "Detect IP"}
                  </Button>
                </div>
                {ipInfo && (ipInfo.local || ipInfo.public) ? (
                  <div className="preset-row">
                    {ipInfo.local ? (
                      <button
                        className={`preset${
                          config.destinationHost === ipInfo.local ? " selected" : ""
                        }`}
                        onClick={() =>
                          updateConfig((c) => (c.destinationHost = ipInfo.local ?? ""))
                        }
                      >
                        LAN {ipInfo.local}
                      </button>
                    ) : null}
                    {ipInfo.public ? (
                      <button
                        className={`preset${
                          config.destinationHost === ipInfo.public ? " selected" : ""
                        }`}
                        onClick={() =>
                          updateConfig((c) => (c.destinationHost = ipInfo.public ?? ""))
                        }
                      >
                        WAN {ipInfo.public}
                      </button>
                    ) : null}
                  </div>
                ) : null}
                <div className="window-list">
                  {listenerEndpoints.length === 0 ? (
                    <p className="hint">Enter a listen port to see the addresses to share.</p>
                  ) : (
                    listenerEndpoints.map((ep, i) => (
                      <div key={ep} className="window-item">
                        <span className="window-proc">Device {i + 1}</span>
                        <span className="window-title">
                          <code>{ep}</code>
                        </span>
                        <Button
                          variant="ghost"
                          onClick={() => navigator.clipboard.writeText(ep)}
                        >
                          Copy
                        </Button>
                      </div>
                    ))
                  )}
                </div>
              </>
            ) : null}
            <Field label="Latency preset">
              <select
                value={config.latencyMs}
                onChange={(e) =>
                  updateConfig((c) => (c.latencyMs = Number(e.target.value)))
                }
              >
                {LATENCY_OPTIONS.map((o) => (
                  <option key={o.value} value={o.value}>
                    {o.label}
                  </option>
                ))}
              </select>
            </Field>
            {!destinationValid ? (
              <p className="hint warn">
                {isListener
                  ? "Enter a listen port (1-65535)."
                  : "Enter the IP and port from your operator."}
              </p>
            ) : null}
          </Card>
        ) : (
          <Card
            title="ST 2110 destinations"
            subtitle="Multicast group addresses and ports the receivers subscribe to."
          >
            <div className="row">
              <Field label="Video IP (2110-20)">
                <input
                  value={config.st2110.videoDestIp}
                  placeholder="239.20.20.20"
                  onChange={(e) =>
                    updateConfig((c) => (c.st2110.videoDestIp = e.target.value))
                  }
                />
              </Field>
              <Field label="Video port">
                <input
                  value={config.st2110.videoDestPort || ""}
                  placeholder="20000"
                  inputMode="numeric"
                  onChange={(e) =>
                    updateConfig(
                      (c) =>
                        (c.st2110.videoDestPort =
                          Number(e.target.value.replace(/\D/g, "")) || 0)
                    )
                  }
                />
              </Field>
            </div>
            <Toggle
              label="Send audio (ST 2110-30)"
              checked={config.st2110.audioEnabled}
              onChange={(v) => updateConfig((c) => (c.st2110.audioEnabled = v))}
            />
            {config.st2110.audioEnabled ? (
              <div className="row">
                <Field label="Audio IP (2110-30)">
                  <input
                    value={config.st2110.audioDestIp}
                    placeholder="239.20.20.30"
                    onChange={(e) =>
                      updateConfig((c) => (c.st2110.audioDestIp = e.target.value))
                    }
                  />
                </Field>
                <Field label="Audio port">
                  <input
                    value={config.st2110.audioDestPort || ""}
                    placeholder="20030"
                    inputMode="numeric"
                    onChange={(e) =>
                      updateConfig(
                        (c) =>
                          (c.st2110.audioDestPort =
                            Number(e.target.value.replace(/\D/g, "")) || 0)
                      )
                    }
                  />
                </Field>
              </div>
            ) : null}
            <div className="row">
              <Field label="Media NIC IP (optional)">
                <input
                  value={config.st2110.interfaceIp}
                  placeholder="OS default route"
                  onChange={(e) =>
                    updateConfig((c) => (c.st2110.interfaceIp = e.target.value))
                  }
                />
              </Field>
              <Field label="PTP domain">
                <input
                  value={config.st2110.ptpDomain}
                  inputMode="numeric"
                  onChange={(e) =>
                    updateConfig(
                      (c) =>
                        (c.st2110.ptpDomain = Math.min(
                          127,
                          Number(e.target.value.replace(/\D/g, "")) || 0
                        ))
                    )
                  }
                />
              </Field>
            </div>
            <Field label="Video payload type (96-127)">
              <input
                value={config.st2110.payloadType}
                inputMode="numeric"
                onChange={(e) =>
                  updateConfig(
                    (c) =>
                      (c.st2110.payloadType =
                        Number(e.target.value.replace(/\D/g, "")) || 0)
                  )
                }
              />
            </Field>
            {!st2110Valid ? (
              <p className="hint warn">
                Enter valid IPv4 destinations and a dynamic payload type (96-127).
              </p>
            ) : null}
          </Card>
        )}

        {/* Identity */}
        <Card title="Identity" subtitle="Shown to the operator so they know who's who.">
          <Field label="Display name">
            <input
              value={config.displayName}
              placeholder="e.g. Idan"
              onChange={(e) =>
                updateConfig((c) => (c.displayName = e.target.value))
              }
            />
          </Field>
          <div className="kv">
            <span>Player ID</span>
            <code>{identity.playerId}</code>
          </div>
          <div className="kv">
            <span>Session ID</span>
            <code className="muted">{identity.sessionId.slice(0, 8)}…</code>
          </div>
        </Card>

        {/* Source */}
        <Card
          title="Source"
          subtitle={
            sourceMode === "window"
              ? "Capturing an application window (WGC)."
              : "Pick the camera or capture card to broadcast."
          }
        >
          <div className="source-head">
            <span className="hint">
              {sourceMode === "window"
                ? selectedWindowId !== null
                  ? "Selected window is highlighted."
                  : "Select a window below."
                : deviceSelected
                ? "Selected device is highlighted."
                : "Select a capture device below."}
            </span>
            <div className="row" style={{ gap: 8 }}>
              {isSt2110 ? null : (
                <Button variant="ghost" onClick={toggleSourceMode}>
                  {sourceMode === "device"
                    ? "Change to window capture"
                    : "Change to camera capture"}
                </Button>
              )}
              <Button
                variant="ghost"
                onClick={sourceMode === "window" ? refreshWindows : refreshDevices}
              >
                Refresh
              </Button>
            </div>
          </div>
          {sourceMode === "window" ? (
            <div className="window-list">
              {windows.length === 0 ? (
                <p className="hint">
                  No capturable windows found. Open the app you want to broadcast,
                  then Refresh.
                </p>
              ) : (
                windows.map((w) => (
                  <button
                    key={w.id}
                    className={`window-item${
                      selectedWindowId === w.id ? " selected" : ""
                    }`}
                    onClick={() => void selectWindow(w)}
                  >
                    <span className="window-proc">{w.processName}</span>
                    <span className="window-title">{w.title}</span>
                  </button>
                ))
              )}
            </div>
          ) : (
            <div className="window-list">
              {videoDevices.length === 0 ? (
                <p className="hint">
                  No capture devices found. Connect a camera or capture card, then
                  Refresh.
                </p>
              ) : (
                videoDevices.map((d) => (
                  <button
                    key={d.id}
                    className={`window-item${
                      config.videoSource.deviceName === d.id ? " selected" : ""
                    }`}
                    onClick={() => selectDevice(d)}
                  >
                    <span className="window-proc">Capture device</span>
                    <span className="window-title">{d.name}</span>
                  </button>
                ))
              )}
            </div>
          )}
        </Card>

        {/* Audio mixer */}
        <Card
          title="Audio mixer"
          subtitle="Mix desktop audio, microphones and per-app audio into the broadcast track."
        >
          {config.audio.sources.length === 0 ? (
            <p className="hint">
              No audio channels — the stream will be video-only. Add a source below.
            </p>
          ) : (
            <div className="mixer">
              {config.audio.sources.map((s) => (
                <div key={s.id} className={`mixer-row${s.muted ? " is-muted" : ""}`}>
                  <div className="mixer-head">
                    <span className="mixer-kind">{AUDIO_KIND_LABEL[s.type]}</span>
                    <button
                      className="mixer-remove"
                      onClick={() => removeAudioSource(s.id)}
                    >
                      Remove
                    </button>
                  </div>
                  {s.type === "microphone" ? (
                    <select
                      value={s.deviceId ?? ""}
                      onChange={(e) =>
                        updateAudioSource(s.id, (x) => {
                          const mic = caps.microphones.find((m) => m.id === e.target.value);
                          x.deviceId = e.target.value || null;
                          x.label = mic?.name ?? "Microphone";
                        })
                      }
                    >
                      <option value="">Default microphone</option>
                      {caps.microphones.map((m) => (
                        <option key={m.id} value={m.id}>
                          {m.name}
                        </option>
                      ))}
                    </select>
                  ) : s.type === "application" ? (
                    <div className="row" style={{ gap: 8, gridTemplateColumns: "1fr auto" }}>
                      <select
                        value={s.processId ?? ""}
                        onChange={(e) =>
                          updateAudioSource(s.id, (x) => {
                            const pid = Number(e.target.value) || null;
                            const proc = audioProcesses.find((p) => p.pid === pid);
                            x.processId = pid;
                            x.label = proc?.name ?? "Application";
                          })
                        }
                      >
                        <option value="">Select an application…</option>
                        {audioProcesses.map((p) => (
                          <option key={p.pid} value={p.pid}>
                            {p.name} (pid {p.pid})
                          </option>
                        ))}
                      </select>
                      <Button variant="ghost" onClick={refreshAudioProcesses}>
                        Refresh
                      </Button>
                    </div>
                  ) : (
                    <span className="hint">
                      Whole-desktop mix (every app + system sounds).
                    </span>
                  )}
                  <div className="mixer-controls">
                    <input
                      className="slider"
                      type="range"
                      min={0}
                      max={2}
                      step={0.05}
                      value={s.gain}
                      disabled={s.muted}
                      onChange={(e) =>
                        updateAudioSource(s.id, (x) => (x.gain = Number(e.target.value)))
                      }
                    />
                    <span className="mixer-gain">{Math.round(s.gain * 100)}%</span>
                    <button
                      className={`mute${s.muted ? " active" : ""}`}
                      onClick={() => updateAudioSource(s.id, (x) => (x.muted = !x.muted))}
                    >
                      {s.muted ? "Muted" : "Mute"}
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}
          <div className="preset-row">
            <button className="preset" onClick={() => addAudioSource("system")}>
              + Desktop audio
            </button>
            <button className="preset" onClick={() => addAudioSource("microphone")}>
              + Microphone
            </button>
            <button className="preset" onClick={() => addAudioSource("application")}>
              + Application
            </button>
          </div>
          <p className="hint">
            Per-application audio uses Windows process loopback (Win10 20H1+). If the
            OS refuses a channel it's skipped with a note in Diagnostics.
          </p>
        </Card>

        {/* Quality */}
        <Card
          title="Broadcast profile"
          subtitle="Resolution, scan mode and rate. Higher profiles need far more bandwidth."
        >
          <div className="preset-row">
            {PRESETS.map((p) => (
              <button
                key={p.id}
                className={`preset${config.presetId === p.id ? " selected" : ""}`}
                onClick={() => onPreset(p.id)}
              >
                {p.label}
              </button>
            ))}
          </div>
          {activeProfile ? (
            <div className="kv">
              <span>Selected</span>
              <code>
                {activeProfile.width}×{activeProfile.height}{" "}
                {activeProfile.scan === "interlaced" ? "i" : "p"}
                {activeProfile.displayRate}
                {isSt2110
                  ? ` · ~${formatBandwidth(activeProfile.st2110Mbps)} raw`
                  : ""}
              </code>
            </div>
          ) : null}
          {isSt2110 ? (
            <p className="hint">
              ST 2110 sends uncompressed video, so bandwidth is fixed by the
              profile regardless of GPU encoders.
            </p>
          ) : (
            <>
              <Toggle
                label="Allow software encoder (CPU heavy)"
                checked={config.encoder.allowSoftware}
                onChange={(v) => updateConfig((c) => (c.encoder.allowSoftware = v))}
              />
              <Toggle
                label="Experimental HEVC (saves bandwidth)"
                disabled={!hevcAvailable}
                checked={config.encoder.experimentalHevc}
                onChange={(v) =>
                  updateConfig((c) => (c.encoder.experimentalHevc = v))
                }
              />
              {!canEncode ? (
                <p className="hint warn">
                  No hardware encoder detected. Update GPU drivers or enable the
                  software encoder above.
                </p>
              ) : null}
            </>
          )}
        </Card>

        {/* Status */}
        <Card title="Status">
          <div className="kv">
            <span>State</span>
            <Pill tone={STATE_TONE[status?.state ?? "idle"] ?? "muted"}>
              {(status?.state ?? "idle").toUpperCase()}
            </Pill>
          </div>
          <div className="kv">
            <span>Auto reconnect</span>
            <Pill tone="ok">ENABLED</Pill>
          </div>
          <div className="kv">
            <span>Source</span>
            <code>{status?.source ?? "—"}</code>
          </div>
          <div className="kv">
            <span>Encoder</span>
            <code>{status?.encoder ?? "—"}</code>
          </div>
          <div className="kv">
            <span>FPS / bitrate</span>
            <code>
              {status?.fps ?? 0} / {status?.bitrate ?? 0} kbps
            </code>
          </div>
          <div className="kv">
            <span>Dropped frames</span>
            <code>{status?.droppedFrames ?? 0}</code>
          </div>
          {status?.lastError ? (
            <p className="hint warn">{status.lastError}</p>
          ) : null}
          {status?.audioWarnings?.map((w, i) => (
            <p key={i} className="hint">
              {w}
            </p>
          ))}
        </Card>

        {/* Capabilities */}
        <Card title="This PC" subtitle="Detected at startup.">
          <div className="kv">
            <span>Capture</span>
            <code>{caps.capture}</code>
          </div>
          <div className="kv">
            <span>Encoders</span>
            <code>{caps.encoders.join(", ") || "none"}</code>
          </div>
          <div className="kv">
            <span>Process audio</span>
            <code>{caps.processAudio ? "yes" : "no (system mix)"}</code>
          </div>
          <div className="kv">
            <span>Discord</span>
            <code>{caps.discordAudio ? "running" : "not detected"}</code>
          </div>
          <div className="kv">
            <span>Microphones</span>
            <code>{caps.microphones.length}</code>
          </div>
          <div className="kv">
            <span>Capture devices</span>
            <code>{caps.videoDevices.length}</code>
          </div>
          <div className="kv">
            <span>GStreamer</span>
            <code>{caps.gstreamerAvailable ? "ready" : "missing"}</code>
          </div>
          <div className="kv">
            <span>ST 2110</span>
            <code>{caps.st2110Ready ? "ready" : "unavailable"}</code>
          </div>
          <div className="kv">
            <span>FFmpeg</span>
            <code>{caps.ffmpegAvailable ? "ready" : "missing"}</code>
          </div>
          <div className="kv">
            <span>OS</span>
            <code>
              {caps.os.name} {caps.os.build ?? ""}
            </code>
          </div>
        </Card>

        {/* Diagnostics */}
        <Card title="Diagnostics" subtitle="Local log (also sent on operator request).">
          <pre className="logs" ref={logRef}>
            {logs.length ? logs.join("\n") : "No logs yet."}
          </pre>
          <Button
            variant="ghost"
            onClick={() => navigator.clipboard.writeText(logs.join("\n"))}
          >
            Copy log
          </Button>
        </Card>
      </main>

      <footer className="controls">
        {isStreaming ? (
          <Button variant="danger" onClick={onStop}>
            Stop
          </Button>
        ) : (
          <Button variant="primary" onClick={onStart} disabled={startDisabled}>
            {isSt2110 ? "Start Broadcast" : "Start Streaming"}
          </Button>
        )}
        {startBlocker && !isStreaming ? <span className="hint">{startBlocker}</span> : null}
      </footer>
    </div>
  );
}
