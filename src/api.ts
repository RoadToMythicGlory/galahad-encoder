// Thin typed wrappers around Tauri commands.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type {
  Capabilities,
  ClientConfig,
  Identity,
  IpInfo,
  MicrophoneInfo,
  ProcessInfo,
  StreamStatus,
  VideoDeviceInfo,
  WindowInfo,
} from "./types";

const isTauriRuntime = () =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const mockConfig: ClientConfig = {
  playerId: "player-dev",
  displayName: "",
  destinationHost: "",
  destinationPort: 0,
  latencyMs: 200,
  srtMode: "caller",
  srtMaxCallers: 1,
  presetId: "1080p60",
  transport: "srt",
  st2110: {
    videoDestIp: "239.20.20.20",
    videoDestPort: 20000,
    audioDestIp: "239.20.20.30",
    audioDestPort: 20030,
    interfaceIp: "",
    payloadType: 96,
    ptpDomain: 0,
    audioEnabled: true,
  },
  videoSource: {
    type: "window",
    deviceName: null,
    windowTitle: null,
    processName: null,
  },
  sourceWindowTitle: null,
  sourceProcessName: null,
  audio: {
    sources: [
      {
        id: "system",
        type: "system",
        enabled: true,
        muted: false,
        gain: 1,
        deviceId: null,
        processId: null,
        label: "Desktop audio",
      },
    ],
  },
  encoder: {
    allowSoftware: true,
    experimentalHevc: false,
  },
  controlChannelUrl: null,
};

let browserConfig = structuredClone(mockConfig) as ClientConfig;
let browserStatus: StreamStatus = {
  state: "idle",
  source: null,
  fps: 0,
  bitrate: 0,
  droppedFrames: 0,
  encoder: null,
  captureBackend: "browser-preview",
  lastError: null,
  audioWarnings: ["Desktop capture is available only in the Tauri app."],
};
let browserLogs = [
  "Running browser preview without the Tauri desktop runtime.",
  "Install Rust/Cargo and use `npm run tauri:dev` for capture and streaming.",
];

const browserCapabilities: Capabilities = {
  capture: "browser-preview",
  encoders: ["libx264"],
  encoderBackends: [
    {
      codec: "h264",
      ffmpegName: "libx264",
      vendor: "software",
      hardware: false,
    },
  ],
  processAudio: false,
  discordAudio: false,
  discordProcesses: [],
  microphones: [
    {
      id: "browser-preview-mic",
      name: "Browser preview microphone",
    },
  ],
  videoDevices: [
    { id: "Preview Camera", name: "Preview Camera" },
    { id: "Preview Capture Card", name: "Preview Capture Card" },
  ],
  gstreamerAvailable: false,
  gstreamerPath: "",
  st2110Ready: false,
  ffmpegAvailable: false,
  ffmpegPath: "",
  os: {
    name: "Browser preview",
    build: null,
  },
};

const browserWindows: WindowInfo[] = [
  {
    id: 1,
    title: "Preview Window",
    processName: "browser-preview",
    pid: 0,
  },
];

const browserVideoDevices: VideoDeviceInfo[] = browserCapabilities.videoDevices;

const browserIdentity: Identity = {
  playerId: browserConfig.playerId,
  sessionId: "browser-preview-session",
  displayName: browserConfig.displayName,
};

function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauriRuntime()) {
    return tauriInvoke<T>(command, args);
  }

  switch (command) {
    case "get_capabilities":
    case "refresh_capabilities":
      return Promise.resolve(browserCapabilities as T);
    case "list_windows":
      return Promise.resolve(browserWindows as T);
    case "list_video_devices":
      return Promise.resolve(browserVideoDevices as T);
    case "detect_ip":
      return Promise.resolve({ public: "203.0.113.10", local: "192.168.1.50" } as T);
    case "list_microphones":
      return Promise.resolve(browserCapabilities.microphones as T);
    case "discord_processes":
      return Promise.resolve(browserCapabilities.discordProcesses as T);
    case "list_audio_processes":
      return Promise.resolve([
        { pid: 1001, name: "obs64.exe" },
        { pid: 1002, name: "chrome.exe" },
      ] as T);
    case "get_config":
      return Promise.resolve(structuredClone(browserConfig) as T);
    case "save_config":
      browserConfig = structuredClone(args?.config as ClientConfig);
      return Promise.resolve(undefined as T);
    case "get_identity":
      return Promise.resolve({
        ...browserIdentity,
        playerId: browserConfig.playerId,
        displayName: browserConfig.displayName,
      } as T);
    case "select_window":
      browserStatus = {
        ...browserStatus,
        source: String(args?.label ?? "Preview Window"),
      };
      return Promise.resolve(undefined as T);
    case "get_status":
      return Promise.resolve(browserStatus as T);
    case "start_stream":
      const browserStartError =
        "Browser preview cannot start desktop capture or SRT streaming.";
      browserStatus = {
        ...browserStatus,
        state: "failed",
        lastError: browserStartError,
      };
      browserLogs = [browserStartError, ...browserLogs];
      return Promise.resolve(undefined as T);
    case "stop_stream":
      browserStatus = { ...browserStatus, state: "idle", lastError: null };
      return Promise.resolve(undefined as T);
    case "restart_stream":
      browserLogs = ["Restart requested in browser preview.", ...browserLogs];
      return Promise.resolve(undefined as T);
    case "switch_quality":
      browserLogs = [`Quality preset changed to ${String(args?.preset ?? "")}.`, ...browserLogs];
      return Promise.resolve(undefined as T);
    case "get_logs":
      return Promise.resolve(browserLogs as T);
    default:
      return Promise.reject(new Error(`Unsupported browser preview command: ${command}`));
  }
}

export const api = {
  getCapabilities: () => invoke<Capabilities>("get_capabilities"),
  refreshCapabilities: () => invoke<Capabilities>("refresh_capabilities"),
  listWindows: () => invoke<WindowInfo[]>("list_windows"),
  listVideoDevices: () => invoke<VideoDeviceInfo[]>("list_video_devices"),
  detectIp: () => invoke<IpInfo>("detect_ip"),
  listMicrophones: () => invoke<MicrophoneInfo[]>("list_microphones"),
  discordProcesses: () => invoke<ProcessInfo[]>("discord_processes"),
  listAudioProcesses: () => invoke<ProcessInfo[]>("list_audio_processes"),
  getConfig: () => invoke<ClientConfig>("get_config"),
  saveConfig: (config: ClientConfig) => invoke<void>("save_config", { config }),
  getIdentity: () => invoke<Identity>("get_identity"),
  selectWindow: (windowId: number, label: string) =>
    invoke<void>("select_window", { windowId, label }),
  getStatus: () => invoke<StreamStatus>("get_status"),
  startStream: () => invoke<void>("start_stream"),
  stopStream: () => invoke<void>("stop_stream"),
  restartStream: () => invoke<void>("restart_stream"),
  switchQuality: (preset: string) => invoke<void>("switch_quality", { preset }),
  getLogs: () => invoke<string[]>("get_logs"),
};
