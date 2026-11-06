.pragma library

var states = ["stack-down", "needs-setup", "disconnected", "authenticating",
              "connecting", "connected", "disconnecting", "error"]

function stateLabel(state) {
  var labels = ["VPN off", "Setup required", "Disconnected", "Authenticating…",
                "Connecting…", "Connected", "Disconnecting…", "Connection error"]
  var index = states.indexOf(state)
  return index < 0 ? "Unknown state · " + state : labels[index]
}

function validStatus(value) {
  return value && typeof value === "object" && !Array.isArray(value)
    && typeof value.state === "string" && value.state.length > 0
}

function gateways(value) {
  if (!Array.isArray(value)) return []
  return value.filter(function(g) {
    return g && typeof g.name === "string" && typeof g.address === "string" && g.address !== ""
  }).map(function(g) { return { value: g.address, label: g.name || g.address } })
}

function boolSetting(value, fallback) {
  if (value === undefined || value === null) return fallback
  if (typeof value === "string") return ["", "false", "0", "no", "off"].indexOf(value.trim().toLowerCase()) < 0
  return !!value
}

function errorLine(text) {
  var lines = String(text || "").split("\n").map(function(s) { return s.trim() })
    .filter(function(s) { return s !== "" })
  for (var i = lines.length - 1; i >= 0; i--) {
    if (lines[i].indexOf("Error:") === 0) return lines[i].slice(6).trim()
  }
  return lines.length ? lines[lines.length - 1] : ""
}

function duration(seconds) {
  var s = Math.max(0, Math.floor(Number(seconds) || 0))
  if (s >= 86400) return Math.floor(s / 86400) + "d " + Math.floor(s % 86400 / 3600) + "h"
  if (s >= 3600) return Math.floor(s / 3600) + "h " + Math.floor(s % 3600 / 60) + "m"
  if (s >= 60) return Math.floor(s / 60) + "m " + s % 60 + "s"
  return s + "s"
}

function bytes(value) {
  var n = Math.max(0, Number(value) || 0)
  if (!isFinite(n)) return "—"
  var units = ["B", "KiB", "MiB", "GiB", "TiB"]
  var unit = 0
  while (n >= 1024 && unit < units.length - 1) { n /= 1024; unit++ }
  return (unit === 0 ? Math.floor(n) : n.toFixed(n < 10 ? 2 : n < 100 ? 1 : 0)) + " " + units[unit]
}
