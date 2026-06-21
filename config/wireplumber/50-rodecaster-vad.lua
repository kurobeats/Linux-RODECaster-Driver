# WirePlumber configuration for RODECaster Virtual Audio Device
# ==============================================================
# Place this file at:
#   ~/.config/wireplumber/main.lua.d/50-rodecaster-vad.lua
#
# This creates persistent device names for each virtual RODECaster channel
# so they appear consistently in PulseAudio/PipeWire applications.

rule = {
  matches = {
    -- Match snd-aloop devices with our naming convention
    { "alsa.card.name", "matches", "Loopback" },
  },
  apply_properties = {
    ["api.alsa.card.name"]      = "RODECaster_VAD",
    ["device.nick"]             = "RODECaster Virtual Audio",
    ["device.description"]      = "RODECaster Virtual Audio Device",
    ["device.icon-name"]        = "audio-card-analog-usb",
    ["device.profile-set"]      = "rodecaster-vad.conf",
  },
}

-- Individual node descriptions for each subdevice
-- Subdevice 0 = System, 1 = Combo1, 2 = Combo2, 3 = Combo3, 4 = Combo4
-- Subdevice 5 = Bluetooth, 6 = SmartPads, 7 = USB1Main, 8 = USB1Chat
-- Subdevice 9 = USB2, 10 = Headset
-- Subdevice 11 = System Out, 12 = Music, 13 = Game, 14 = VirtualA, 15 = VirtualB

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

playback_names = {
  [0] = "System Out",
  [1] = "Music Out",
  [2] = "Game Out",
  [3] = "Virtual A Out",
  [4] = "Virtual B Out",
}

for i, name in pairs(capture_names) do
  table.insert(rule, {
    matches = {
      { "alsa.card.name", "matches", "Loopback" },
      { "alsa.pcm.stream", "equals", "capture" },
      { "alsa.pcm.device", "equals", i },
    },
    apply_properties = {
      ["node.name"]        = "rodecaster_capture_" .. i,
      ["node.description"] = "RODECaster " .. name,
      ["node.nick"]        = name,
      ["media.class"]      = "Audio/Source",
      ["priority.session"] = 800,
    },
  })
end

for i, name in pairs(playback_names) do
  table.insert(rule, {
    matches = {
      { "alsa.card.name", "matches", "Loopback" },
      { "alsa.pcm.stream", "equals", "playback" },
      { "alsa.pcm.device", "equals", i + 11 },
    },
    apply_properties = {
      ["node.name"]        = "rodecaster_playback_" .. i,
      ["node.description"] = "RODECaster " .. name,
      ["node.nick"]        = name,
      ["media.class"]      = "Audio/Sink",
      ["priority.session"] = 800,
    },
  })
end
