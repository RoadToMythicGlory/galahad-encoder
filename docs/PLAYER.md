# Galahad Encoder — Player Guide

You only need two things from your operator: a **Server IP** and a **Port**
(for example `1.2.3.4:9003`). The app handles everything else.

## 1. Install

Run the installer your operator sent you. FFmpeg is bundled — you do not install
anything else.

## 2. Set the destination

In the **Destination** card enter the **Server IP** and **Port** the operator gave
you, and pick a latency preset (use *Normal* unless told otherwise).

## 3. Enter your name

In the **Identity** card type your **Display name** (e.g. your gamer tag) so the
operator can see who you are.

## 4. Pick your game window

In the **Source** card, open your game first, then click **Refresh** and select
the game window. Tip: run the game in **borderless / fullscreen-windowed** mode
for the most reliable capture.

## 5. Choose audio

In the **Audio** card you can include:
- Game / app audio
- Discord
- Microphone (pick the device)

Use the sliders to balance levels, or **Mute** a source.

## 6. Pick quality

`1080p30` is a good default. Use `720p30` if your upload is limited, `1080p60`
for fast games if your upload can handle it.

## 7. Start

Click **Start Streaming**. The status turns to **STREAMING** when you are live.

- **Auto reconnect is always on** — if your game closes, Discord crashes, or your
  internet blips, the app reconnects by itself.
- The **Status** card shows your state, FPS, bitrate, and dropped frames.
- If something looks wrong, the **Diagnostics** card has a log you can copy and
  send to your operator.

## Troubleshooting

| Symptom | Fix |
|--------|-----|
| Start is disabled | Make sure a source window is selected and the IP/port are filled in. |
| "No hardware encoder detected" | Update your GPU drivers, or enable the software encoder in Quality (uses more CPU). |
| Black video | Use borderless/windowed mode; some anti-cheat blocks exclusive-fullscreen capture. |
| No game audio | Re-select the game window; check the Game audio toggle isn't muted. |
