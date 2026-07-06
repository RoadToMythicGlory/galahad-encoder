// Mirror of the Rust serde (camelCase) payloads exposed via Tauri commands.

export type StreamState =
  | "idle"
  | "starting"
  | "streaming"
  | "degraded"
  | "reconnecting"
  | "failed";

export interface OsInfo {
  name: string;
  build: string | null;
}

export interface EncoderBackend {
  codec: "h264" | "hevc";
  ffmpegName: string;
  vendor: "nvidia" | "intel" | "amd" | "software";
  hardware: boolean;
}

export interface ProcessInfo {
  pid: number;
  name: string;
}

export interface MicrophoneInfo {
  id: string;
  name: string;
}

export interface Capabilities {
  capture: string;
  encoders: string[];
  encoderBackends: EncoderBackend[];
  processAudio: boolean;
  discordAudio: boolean;
  discordProcesses: ProcessInfo[];
  microphones: MicrophoneInfo[];
  videoDevices: VideoDeviceInfo[];
  gstreamerAvailable: boolean;
  gstreamerPath: string;
  st2110Ready: boolean;
  ffmpegAvailable: boolean;
  ffmpegPath: string;
  os: OsInfo;
}

export interface WindowInfo {
  id: number;
  title: string;
  processName: string;
  pid: number;
}

export interface VideoDeviceInfo {
  /// Stable DirectShow friendly name (accepted by FFmpeg dshow + GStreamer).
  id: string;
  name: string;
}

export type VideoSourceKind = "window" | "device";

export interface VideoSourceConfig {
  type: VideoSourceKind;
  deviceName: string | null;
  windowTitle: string | null;
  processName: string | null;
}

export type TransportKind = "srt" | "st2110";

export type SrtMode = "caller" | "listener";

export interface IpInfo {
  public: string | null;
  local: string | null;
}

export interface St2110Config {
  videoDestIp: string;
  videoDestPort: number;
  audioDestIp: string;
  audioDestPort: number;
  interfaceIp: string;
  payloadType: number;
  ptpDomain: number;
  audioEnabled: boolean;
}

export interface Identity {
  playerId: string;
  sessionId: string;
  displayName: string;
}

export type AudioSourceKind = "system" | "microphone" | "application";

export interface AudioSource {
  /// Stable id used as the React key and to address the row in updates.
  id: string;
  type: AudioSourceKind;
  enabled: boolean;
  muted: boolean;
  /// 0.0 - 2.0 linear gain (1.0 = unity).
  gain: number;
  /// Microphone endpoint id (null = default device).
  deviceId: string | null;
  /// Target process id for per-application capture.
  processId: number | null;
  /// Display label (device / app name).
  label: string | null;
}

export interface AudioConfig {
  sources: AudioSource[];
}

export interface EncoderConfig {
  allowSoftware: boolean;
  experimentalHevc: boolean;
}

export interface ClientConfig {
  playerId: string;
  displayName: string;
  destinationHost: string;
  destinationPort: number;
  latencyMs: number;
  srtMode: SrtMode;
  srtMaxCallers: number;
  presetId: string;
  transport: TransportKind;
  st2110: St2110Config;
  videoSource: VideoSourceConfig;
  sourceWindowTitle: string | null;
  sourceProcessName: string | null;
  audio: AudioConfig;
  encoder: EncoderConfig;
  controlChannelUrl: string | null;
}

export interface StreamStatus {
  state: StreamState;
  source: string | null;
  fps: number;
  bitrate: number;
  droppedFrames: number;
  encoder: string | null;
  captureBackend: string | null;
  lastError: string | null;
  audioWarnings: string[];
}

export type ScanMode = "progressive" | "interlaced";

export interface QualityPreset {
  id: string;
  label: string;
  width: number;
  height: number;
  scan: ScanMode;
  /// Actual frames per second delivered to the encoder / payloader.
  frameRate: number;
  /// Rate as it appears in the profile name (field rate when interlaced).
  displayRate: number;
  /// Approximate uncompressed ST 2110-20 bandwidth in Mbit/s.
  st2110Mbps: number;
}

export const PRESETS: QualityPreset[] = [
  { id: "4Kp60", label: "4K p60", width: 3840, height: 2160, scan: "progressive", frameRate: 60, displayRate: 60, st2110Mbps: 9953 },
  { id: "4Kp50", label: "4K p50", width: 3840, height: 2160, scan: "progressive", frameRate: 50, displayRate: 50, st2110Mbps: 8294 },
  { id: "1080p60", label: "1080 p60", width: 1920, height: 1080, scan: "progressive", frameRate: 60, displayRate: 60, st2110Mbps: 2488 },
  { id: "1080p50", label: "1080 p50", width: 1920, height: 1080, scan: "progressive", frameRate: 50, displayRate: 50, st2110Mbps: 2074 },
  { id: "4Ki60", label: "4K i60", width: 3840, height: 2160, scan: "interlaced", frameRate: 30, displayRate: 60, st2110Mbps: 4977 },
  { id: "4Ki50", label: "4K i50", width: 3840, height: 2160, scan: "interlaced", frameRate: 25, displayRate: 50, st2110Mbps: 4147 },
  { id: "1080i60", label: "1080 i60", width: 1920, height: 1080, scan: "interlaced", frameRate: 30, displayRate: 60, st2110Mbps: 1244 },
  { id: "1080i50", label: "1080 i50", width: 1920, height: 1080, scan: "interlaced", frameRate: 25, displayRate: 50, st2110Mbps: 1037 },
];
