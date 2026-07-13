// Humanized formatting, mirroring apps/gpwidget/src/ux.rs — keep the two in sync.
.pragma library

function formatBytes(bytes) {
    const units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let value = bytes;
    let unit = 0;

    while (value >= 1024 && unit < units.length - 1) {
        value /= 1024;
        unit += 1;
    }

    if (unit === 0)
        return bytes + " B";
    if (value < 10)
        return value.toFixed(2) + " " + units[unit];
    if (value < 100)
        return value.toFixed(1) + " " + units[unit];
    return value.toFixed(0) + " " + units[unit];
}

// Two most significant units: "2d 3h", "3h 24m", "24m 36s", "36s".
function formatDuration(secs) {
    const days = Math.floor(secs / 86400);
    const hours = Math.floor((secs % 86400) / 3600);
    const minutes = Math.floor((secs % 3600) / 60);
    const seconds = Math.floor(secs % 60);

    if (days > 0)
        return days + "d " + hours + "h";
    if (hours > 0)
        return hours + "h " + minutes + "m";
    if (minutes > 0)
        return minutes + "m " + seconds + "s";
    return seconds + "s";
}

// "in 11h 23m (07:32)"; adds the weekday when not today.
function formatExpiry(expiresAt, nowSecs) {
    const date = new Date(expiresAt * 1000);
    const now = new Date(nowSecs * 1000);

    const sameDay = date.toDateString() === now.toDateString();
    const hhmm = date.toLocaleTimeString(Qt.locale().name.replace("_", "-"), {
        hour: "2-digit",
        minute: "2-digit",
        hour12: false
    });
    const absolute = sameDay ? hhmm : date.toLocaleDateString(undefined, { weekday: "short" }) + " " + hhmm;

    if (expiresAt <= nowSecs)
        return "expired (" + absolute + ")";
    return "in " + formatDuration(expiresAt - nowSecs) + " (" + absolute + ")";
}
