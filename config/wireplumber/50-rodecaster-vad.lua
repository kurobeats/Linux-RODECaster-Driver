-- WirePlumber configuration for RODECaster Virtual Audio Device
-- ==============================================================
-- Place this file at:
--   ~/.config/wireplumber/main.lua.d/50-rodecaster-vad.lua
--
-- This renames the snd-aloop loopback card and each of its 16 subdevices
-- so they appear with consistent, human-readable names in PipeWire
-- applications (pavucontrol, qpwgraph, OBS, etc.).
--
-- Subdevice layout (matches the Windows RODECaster VAD topology):
--   Capture 0-10  → 11 input channels (System In, Combo1-4 In,
--                     Bluetooth In, SmartPads In, USB1 Main In,
--                     USB1 Chat In, USB2 In, Headset In)
--   Playback 11-15 → 5 output channels (System Out, Music Out,
--                     Game Out, Virtual A Out, Virtual B Out)

-- Capture (Input) channel names — subdevices 0 through 10
capture_names = {
  [0]  = "System In",
  [1]  = "Combo 1 In",
  [2]  = "Combo 2 In",
  [3]  = "Combo 3 In",
  [4]  = "Combo 4 In",
  [5]  = "Bluetooth In",
  [6]  = "Smart Pads In",
  [7]  = "USB1 Main In",
  [8]  = "USB1 Chat In",
  [9]  = "USB2 In",
  [10] = "Headset In",
}

-- Playback (Output) channel names — subdevices 11 through 15
playback_names = {
  [0] = "System Out",
  [1] = "Music Out",
  [2] = "Game Out",
  [3] = "Virtual A Out",
  [4] = "Virtual B Out",
}

-- Ensure the ALSA monitor rules table exists (base config creates it)
alsa_monitor.rules = alsa_monitor.rules or {}

-- Card-level rule: renames the snd-aloop card to "RODECaster_VAD"
table.insert(alsa_monitor.rules, {
  matches = {
    { "alsa.card.name", "matches", "Loopback" },
  },
  apply_properties = {
    ["api.alsa.card.name"]      = "RODECaster_VAD",
    ["device.nick"]             = "RODECaster Virtual Audio",
    ["device.description"]      = "RODECaster Virtual Audio Device",
    ["device.icon-name"]        = "audio-card-analog-usb",
  },
})

-- Per-subdevice rules for capture (input) channels
-- Matches snd-aloop PCM nodes like "alsa_input...Loopback...DEV=0"
for i = 0, 10 do
  local name = capture_names[i]
  table.insert(alsa_monitor.rules, {
    matches = {
      { "alsa.card.name",    "matches", "Loopback" },
      { "alsa.pcm.stream",   "equals",  "capture" },
      { "alsa.pcm.device",   "equals",  tostring(i) },
    },
    apply_properties = {
      ["node.name"]           = "alsa_input.rodecaster_vad_" .. i,
      ["node.description"]    = "RODECaster " .. name,
      ["node.nick"]           = name,
      ["media.class"]         = "Audio/Source",
      ["priority.session"]    = 800,
    },
  })
end

-- Per-subdevice rules for playback (output) channels
-- snd-aloop playback subdevices start at device 11
for i = 0, 4 do
  local name = playback_names[i]
  local dev = i + 11
  table.insert(alsa_monitor.rules, {
    matches = {
      { "alsa.card.name",    "matches", "Loopback" },
      { "alsa.pcm.stream",   "equals",  "playback" },
      { "alsa.pcm.device",   "equals",  tostring(dev) },
    },
    apply_properties = {
      ["node.name"]           = "alsa_output.rodecaster_vad_" .. i,
      ["node.description"]    = "RODECaster " .. name,
      ["node.nick"]           = name,
      ["media.class"]         = "Audio/Sink",
      ["priority.session"]    = 800,
    },
  })
end
