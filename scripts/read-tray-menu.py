"""Read the real native tray menu owned by one isolated desktop-test process."""

import json
import os
import sys
import time
import xml.etree.ElementTree as ET

from gi.repository import Gio, GLib


def read_menu(pid):
    if not os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
        raise RuntimeError("An explicit test session bus is required.")
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    deadline = time.monotonic() + 3

    def call(destination, path, interface, method, parameters=None):
        remaining = int((deadline - time.monotonic()) * 1000)
        if remaining <= 0:
            raise RuntimeError("Native tray inspection timed out.")
        return bus.call_sync(
            destination, path, interface, method, parameters, None,
            Gio.DBusCallFlags.NONE, min(remaining, 500), None,
        ).unpack()

    names = call(
        "org.freedesktop.DBus", "/org/freedesktop/DBus",
        "org.freedesktop.DBus", "ListNames",
    )[0]
    for owner in names:
        if not owner.startswith(":"):
            continue
        try:
            owner_pid = call(
                "org.freedesktop.DBus", "/org/freedesktop/DBus",
                "org.freedesktop.DBus", "GetConnectionUnixProcessID",
                GLib.Variant("(s)", (owner,)),
            )[0]
        except GLib.Error:
            continue
        if owner_pid != pid:
            continue
        # Inspect only objects exported by the selected test process, with
        # bounded depth/work even if a library changes its D-Bus object tree.
        pending = [("/", 0)]
        visited = set()
        while pending and len(visited) < 32:
            path, depth = pending.pop()
            if path in visited or depth > 8:
                continue
            visited.add(path)
            try:
                node = ET.fromstring(call(
                    owner, path, "org.freedesktop.DBus.Introspectable", "Introspect",
                )[0])
            except GLib.Error:
                continue
            if any(item.get("name") == "com.canonical.dbusmenu"
                   for item in node.findall("interface")):
                _, layout = call(
                    owner, path, "com.canonical.dbusmenu", "GetLayout",
                    GLib.Variant("(iias)", (0, 1, ["label", "enabled"])),
                )
                return [
                    {"id": item_id, "label": properties.get("label", ""),
                     "enabled": properties.get("enabled", True)}
                    for item_id, properties, _ in layout[2]
                ]
            for child in node.findall("node"):
                name = child.get("name", "")
                if name:
                    child_path = name if name.startswith("/") else path.rstrip("/") + "/" + name
                    pending.append((child_path, depth + 1))
    raise RuntimeError("The isolated test app did not export a native tray menu.")


if __name__ == "__main__":
    try:
        process_id = int(sys.argv[1])
        if process_id <= 0:
            raise ValueError("A positive process ID is required.")
        print(json.dumps(read_menu(process_id)))
    except (IndexError, ValueError, RuntimeError, GLib.Error, ET.ParseError):
        sys.exit("Could not read the native tray menu for the isolated test app.")
