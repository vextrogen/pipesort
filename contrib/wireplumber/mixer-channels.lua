lutils = require ("linking-utils")
log = Log.open_topic ("s-linking")

-- Lua patterns, matched against app name, binary and node name
local CHAT = { "^WEBRTC VoiceEngine$", "[Tt]eam[Ss]peak", "^[Dd]iscord$", "^vesktop$" }
local MUSIC = { "[Ss]potify", "[Tt]idal", "[Ss]trawberry", "[Rr]hythmbox", "[Ee]lisa" }
local GAME = { "%.exe$", "^wine", "steam_app_" }

local function matches (props, patterns)
  for _, key in ipairs { "application.name", "application.process.binary", "node.name" } do
    local v = props [key]
    if v then
      for _, p in ipairs (patterns) do
        if v:find (p) then return true end
      end
    end
  end
  return false
end

SimpleEventHook {
  name = "linking/mixer-channels",
  after = { "linking/find-defined-target", "linking/find-filter-target" },
  before = "linking/find-default-target",
  interests = {
    EventInterest { Constraint { "event.type", "=", "select-target" } },
  },
  execute = function (event)
    local source, om, si, si_props, si_flags, target =
        lutils:unwrap_select_target_event (event)

    if target then return end                                   -- app/user already picked a sink
    if si_props ["media.class"] ~= "Stream/Output/Audio" then return end  -- playback only, never mics
    if si_props ["node.link-group"] then return end             -- our loopbacks, filters, echo-cancel

    local sink = "sink_app"
    if matches (si_props, CHAT) then sink = "sink_chat"
    elseif matches (si_props, MUSIC) then sink = "sink_music"
    elseif matches (si_props, GAME) then sink = "sink_game" end

    local t = om:lookup { type = "SiLinkable", Constraint { "node.name", "=", sink } }
    if t then
      log:info (si, "mixer-channels: -> " .. sink)
      event:set_data ("target", t)
    end
  end
}:register ()
