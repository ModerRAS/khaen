#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""motu_meters.py -- MOTU UltraLite-mk5 output level meters over CueMix 5's USB-CDC WebSocket.

Why this works
--------------
The UltraLite-mk5 exposes a USB CDC-ECM network interface (Linux: enx*) and, in addition to
CueMix 5's normal control traffic, broadcasts a JSON beacon on UDP 1280 to 255.255.255.255:1280
so the app can find it.  The app then opens a plain WebSocket to ws://<device-ip>:1280/ and
speaks the "datastore" message protocol.  One message id is special: id=6000 is a *meter
frame*, sent by the device at ~30 Hz:

    id:u16 BE = 6000 | index:u16 BE (unused/0) | payload[N] bytes
    payload[i] = peak for meter slot i, 0..255 in 0.5 dB steps, dB = -raw/2  (pre-trim)
    slot count = 128 (CueMix 5 meterstore.js allocates 128; datastore.js recv() -> meterStore.set(data[4:]))

Output meter slot indices are the DSP patch numbers from CueMix 5 dev.js (PatchBase.dsp = 46):

    analogOuts[]  dsp = 46,47,48,49,54,55,56,57,62,63   (Main1/2, Line3..Line10)
    phonesOut[0]  dsp = 64 (Phones1), 65 (Phones2)

i.e. main/line outputs are *not* contiguous (the USB host-out patch occupies 50..53 and 58..61).
The analog input meters and other DSP taps live at other slots (see --list-all for the raw table).

This tool is READ-ONLY: it only opens the WebSocket and reads meter frames.  It never writes the
datastore, so it cannot change routing / mute / levels and cannot interrupt USB audio playback.

Typical use (on the dsp-host board, root, with the enx* link up):

    # auto-discover device from the UDP-1280 beacon, watch output meters for 10 s
    python3 motu_meters.py --discover --iface enx0001f2fe7e6f --duration 10

    # known device IP, record 30 Hz meter frames for later alignment with host reports
    python3 motu_meters.py --host 169.254.111.126 --jsonl meters.jsonl --duration 60 --quiet

    # verification mode: exit 0 only if a watched output actually moved above -55 dBFS
    python3 motu_meters.py --host 169.254.111.126 --watch Main1,Main2 --threshold -55 --duration 8

Exit codes: 0 ok(all requested checks pass) | 2 usage/connection error | 3 watched meters never moved.
"""
import argparse
import base64
import json
import os
import select
import socket
import struct
import sys
import time

VERSION = "motu-meters/1.0"


def _graceful_term(signum, frame):
    """SIGTERM -> same graceful path as Ctrl-C (background jobs ignore SIGINT, so the
    measurement harness stops us with SIGTERM and still gets the summary)."""
    raise KeyboardInterrupt


try:
    import signal as _signal
    _signal.signal(_signal.SIGTERM, _graceful_term)
except Exception:
    pass

# ---- protocol constants (from the un-packed CueMix 5 app: src/datastore.js, src/meterstore.js,
#      src/dev.js; see scratch/lead/cuemix-proto/) -------------------------------------------
METERS_ID = 6000
PROXY_ID = 0xFFFE
PROXY_STATUS_ID = 0xFFFF
METER_SLOTS = 128

# PatchBase.dsp = 46; dev.js DeviceProps_ul analogOuts/phonesOut -> dsp slot numbers.
OUTPUT_METERS = [
    ("Main1", 46), ("Main2", 47),
    ("Line3", 48), ("Line4", 49),
    ("Line5", 54), ("Line6", 55),
    ("Line7", 56), ("Line8", 57),
    ("Line9", 62), ("Line10", 63),
    ("Phones1", 64), ("Phones2", 65),
]
OUTPUT_INDEX = dict(OUTPUT_METERS)

# Raw slot table for --list-all (patch semantics; holes documented as unknown).
DSP_TAP_LABELS = dict(OUTPUT_METERS)
for i, name in enumerate(["DSPin0", "DSPin1", "DSPin2", "DSPin3"]):
    DSP_TAP_LABELS[(4 + i)] = name          # analogIns[0..1] postfx = dsp 4,5
for i, name in enumerate(["DSPin2", "DSPin3"]):
    pass
ANALOG_IN_POSTFX = {"Mic1": 4, "Mic2": 5, "Line3in": 6, "Line4in": 7,
                    "Line5in": 12, "Line6in": 13, "Line7in": 14, "Line8in": 15}
DSP_TAP_LABELS.update(ANALOG_IN_POSTFX)

DEFAULT_WATCH = ["Main1", "Main2"]


def db_from_raw(raw):
    """peak byte -> dBFS (0.5 dB steps; raw 255 is the '-' floor used by meterstore)."""
    return -raw / 2.0


def fmt_db(raw):
    if raw >= 250:
        return "  -inf"
    return "%6.1f" % db_from_raw(raw)


# ------------------------------------------------------------------ WebSocket client
class WsClient:
    """Minimal dependency-free WebSocket (text/binary) client for ws://host:port/."""

    def __init__(self, host, port, path="/", timeout=5.0, verbose=False):
        self.host, self.port, self.path = host, port, path
        self.timeout, self.verbose = timeout, verbose
        self.sock = None
        self.buf = b""

    def open(self):
        self.sock = socket.create_connection((self.host, self.port), timeout=self.timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        req = ("GET %s HTTP/1.1\r\nHost: %s:%d\r\nUpgrade: websocket\r\n"
               "Connection: Upgrade\r\nSec-WebSocket-Key: %s\r\n"
               "Sec-WebSocket-Version: 13\r\nUser-Agent: %s\r\n\r\n"
               % (self.path, self.host, self.port, key, VERSION))
        self.sock.sendall(req.encode())
        resp = b""
        while b"\r\n\r\n" not in resp:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise IOError("closed during websocket handshake")
            resp += chunk
            if len(resp) > 65536:
                raise IOError("handshake response too large")
        head, _, rest = resp.partition(b"\r\n\r\n")
        status = head.split(b"\r\n", 1)[0].decode("latin1")
        if " 101" not in status:
            raise IOError("websocket upgrade refused: %s" % status)
        self.buf = rest
        return self

    def close(self):
        try:
            self.sock.sendall(struct.pack("!BB", 0x88, 0x80) + os.urandom(4))
        except Exception:
            pass
        try:
            self.sock.close()
        except Exception:
            pass

    def _exact(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise IOError("connection closed by peer")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def poll(self, timeout):
        """Return list of binary/text payloads available within timeout seconds."""
        if self.sock is None:
            return []
        r, _, _ = select.select([self.sock], [], [], timeout)
        if not r:
            return []
        b0, b1 = self._exact(2)
        opcode = b0 & 0x0F
        masked, ln = b1 & 0x80, b1 & 0x7F
        if ln == 126:
            ln = struct.unpack("!H", self._exact(2))[0]
        elif ln == 127:
            ln = struct.unpack("!Q", self._exact(8))[0]
        mask = self._exact(4) if masked else None
        data = self._exact(ln) if ln else b""
        if mask:
            data = bytes(c ^ mask[i & 3] for i, c in enumerate(data))
        if opcode == 0x9:  # ping -> pong
            m = os.urandom(4)
            self.sock.sendall(struct.pack("!BB", 0x8A, 0x80 | len(data)) + m +
                              bytes(c ^ m[i & 3] for i, c in enumerate(data)))
            return []
        if opcode in (0x8, 0xA):
            raise IOError("websocket closed by peer")
        return [data]


# ----------------------------------------------------------------------- discovery
def discover_beacon(iface=None, seconds=6.0):
    """Passively listen for the UDP-1280 JSON beacon (no packets sent). Root needs AF_PACKET."""
    if iface is None:
        import glob
        cands = [p.split("/")[-1] for p in glob.glob("/sys/class/net/enx*")]
        iface = cands[0] if cands else None
    if iface is None:
        return None
    try:
        s = socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(0x0003))
        s.bind((iface, 0))
        s.settimeout(1.0)
    except Exception as exc:
        print("[discover] raw socket on %s failed: %s" % (iface, exc))
        return None
    print("[discover] listening %ds for UDP-1280 beacon on %s (passive)" % (seconds, iface))
    end = time.time() + seconds
    try:
        while time.time() < end:
            try:
                pkt = s.recv(65535)
            except socket.timeout:
                continue
            if pkt[12:14] != b"\x08\x00" or len(pkt) < 34 or pkt[23] != 17:
                continue
            ihl = (pkt[14] & 0x0f) * 4
            sport = (pkt[14 + ihl] << 8) | pkt[14 + ihl + 1]
            dport = (pkt[14 + ihl + 2] << 8) | pkt[14 + ihl + 3]
            if sport != 1280 and dport != 1280:
                continue
            try:
                return json.loads(pkt[14 + ihl + 8:].decode("utf-8"))
            except Exception:
                continue
    finally:
        s.close()
    return None


# ---------------------------------------------------------------------- meter frames
def parse_meter_frame(msg):
    """Return list of raw peaks if this is an id=6000 meter frame, else None."""
    if len(msg) < 4:
        return None
    mid = struct.unpack("!H", msg[:2])[0]
    if mid != METERS_ID:
        return None
    return list(msg[4:4 + METER_SLOTS])


def run(args):
    if args.list_indices:
        print("MOTU UltraLite-mk5 meter slots (CueMix 5 PatchBase.dsp=46):")
        for name, idx in OUTPUT_METERS:
            print("  %-8s %3d" % (name, idx))
        print("\nother known DSP taps:")
        for name, idx in sorted(ANALOG_IN_POSTFX.items(), key=lambda kv: kv[1]):
            print("  %-8s %3d" % (name, idx))
        return 0

    host, port = args.host, args.port
    if args.discover or host is None:
        beacon = discover_beacon(args.discover_iface, args.discover_timeout)
        if beacon is None:
            print("[discover] no UDP-1280 beacon seen; pass --host <ip> (known: 169.254.111.126)")
            return 2
        print("[discover] %s" % json.dumps(beacon, sort_keys=True))
        host = beacon.get("ip") or host
    if not host:
        print("[error] no device host")
        return 2
    if port is None:
        port = 1280

    if args.slots:
        # arbitrary patch-number slots (e.g. 32-43 = i2s taps, 0-22 = TDM taps) for diagnostics
        try:
            slots = [int(s) for s in args.slots.split(",") if s.strip()]
        except ValueError:
            print("[error] bad --slots list")
            return 2
        for s in slots:
            if not 0 <= s < METER_SLOTS:
                print("[error] slot %d out of 0..%d" % (s, METER_SLOTS - 1))
                return 2
            name = "slot%d" % s
            if name not in OUTPUT_INDEX:
                OUTPUT_METERS.append((name, s))
                OUTPUT_INDEX[name] = s

    watch = args.watch
    if watch:
        unknown = [w for w in watch if w not in OUTPUT_INDEX]
        if unknown:
            print("[error] unknown --watch names: %s (known: %s)"
                  % (", ".join(unknown), ", ".join(n for n, _ in OUTPUT_METERS)))
            return 2
    else:
        watch = list(DEFAULT_WATCH)

    print("[connect] ws://%s:%d%s  slots=%s  watch=%s"
          % (host, port, args.path, "all" if args.all else "outputs", ",".join(watch)))
    tp = WsClient(host, port, args.path, timeout=args.timeout, verbose=args.verbose).open()
    print("[connect] websocket up")

    out = None
    if args.jsonl:
        out = open(args.jsonl, "w", encoding="utf-8")
        meta = {"kind": "meta", "schema": VERSION, "host": host, "port": port,
                "t_start": time.time(), "watch": watch,
                "output_meters": OUTPUT_METERS}
        out.write(json.dumps(meta) + "\n")

    t0 = time.time()
    t_end = t0 + args.duration if args.duration and args.duration > 0 else None
    last_print = 0.0
    frames = 0
    frames_with_signal = 0
    last_seen = time.time()
    peak_raw = {name: 255 for name, _ in OUTPUT_METERS}
    peak_raw_any = {i: 255 for i in range(METER_SLOTS)}
    last_raw = [255] * METER_SLOTS
    moving = {name: False for name in watch}

    try:
        while True:
            now = time.time()
            if t_end is not None and now >= t_end:
                break
            if args.idle_stop and (now - last_seen) >= args.idle_stop:
                print("[stop] idle %.1fs without meter frames" % args.idle_stop)
                break
            msgs = tp.poll(min(0.2, max(0.01, (t_end - now) if t_end else 0.2)))
            for msg in msgs:
                raw = parse_meter_frame(msg)
                if raw is None:
                    continue
                last_seen = time.time()
                frames += 1
                seen_signal = False
                for name, idx in OUTPUT_METERS:
                    if idx < len(raw):
                        last_raw[idx] = raw[idx]
                        if raw[idx] < peak_raw[name]:
                            peak_raw[name] = raw[idx]
                for i, v in enumerate(raw):
                    if v < peak_raw_any[i]:
                        peak_raw_any[i] = v
                for name in watch:
                    idx = OUTPUT_INDEX[name]
                    if idx < len(raw) and raw[idx] < 250:
                        seen_signal = True
                        if args.threshold is None or db_from_raw(raw[idx]) > args.threshold:
                            moving[name] = True
                if seen_signal:
                    frames_with_signal += 1
                if out is not None:
                    rec = {"kind": "meters", "t": time.time(), "n": frames,
                           "raw": {name: raw[idx] for name, idx in OUTPUT_METERS if idx < len(raw)},
                           "db": {name: (None if raw[idx] >= 250 else db_from_raw(raw[idx]))
                                  for name, idx in OUTPUT_METERS if idx < len(raw)}}
                    if args.all:
                        rec["raw_all"] = list(raw)
                    out.write(json.dumps(rec, separators=(",", ":")) + "\n")
                if not args.quiet:
                    now2 = time.time()
                    if now2 - last_print >= args.print_interval:
                        last_print = now2
                        parts = []
                        for name, idx in OUTPUT_METERS:
                            if idx < len(raw) and (raw[idx] < 250 or name in watch):
                                parts.append("%s=%s" % (name, fmt_db(raw[idx]).strip()))
                        print("t=%7.2fs n=%-6d %s" % (now2 - t0, frames, " ".join(parts)))
    except KeyboardInterrupt:
        print("\n[stop] interrupted")
    finally:
        tp.close()
        if out is not None:
            out.flush()
            out.close()

    dur = time.time() - t0
    print("\n== summary: %d meter frames in %.1fs (%.1f Hz) ==" % (frames, dur, frames / max(dur, 1e-9)))
    print("  %-8s %5s %9s %9s" % ("meter", "slot", "peak_dB", "last_dB"))
    for name, idx in OUTPUT_METERS:
        pr = peak_raw[name]
        print("  %-8s %5d %9s %9s%s"
              % (name, idx, ("-inf" if pr >= 250 else "%.1f" % db_from_raw(pr)),
                 fmt_db(last_raw[idx]).strip(), "   <-- moving" if moving.get(name) else ""))
    if args.all:
        moved = [(i, peak_raw_any[i]) for i in range(METER_SLOTS) if peak_raw_any[i] < 246]
        print("\n  slots with signal (peak <= -1 dB of floor):")
        for i, pr in moved:
            label = DSP_TAP_LABELS.get(i, "slot%d" % i)
            print("    %3d %-8s %.1f dB" % (i, label, db_from_raw(pr)))
    if out is not None:
        print("\n[jsonl] wrote %s" % args.jsonl)

    if args.motion_check:
        active = [n for n in watch if moving[n]]
        if not active:
            print("[check] FAIL: none of %s moved above %.1f dBFS during %.1fs"
                  % (",".join(watch), args.threshold, dur))
            return 3
        print("[check] PASS: %s moved" % ",".join(active))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(
        description="MOTU UltraLite-mk5 output level meters (CueMix 5 WS, id=6000). READ-ONLY.")
    ap.add_argument("--host", default=None, help="device IP (default: auto-discover via UDP-1280)")
    ap.add_argument("--port", type=int, default=None, help="WS port (default 1280)")
    ap.add_argument("--path", default="/", help="WS request path (default /)")
    ap.add_argument("--discover", action="store_true", help="discover via UDP-1280 beacon first")
    ap.add_argument("--discover-iface", default=None, help="interface for beacon listen (enx*)")
    ap.add_argument("--discover-timeout", type=float, default=6.0)
    ap.add_argument("--timeout", type=float, default=5.0, help="TCP connect/handshake timeout")
    ap.add_argument("--duration", type=float, default=10.0, help="seconds to record (0 = forever)")
    ap.add_argument("--idle-stop", type=float, default=0.0, help="stop after N idle seconds")
    ap.add_argument("--jsonl", default=None, help="write every meter frame as JSON lines")
    ap.add_argument("--all", action="store_true", help="record/summarize all 128 slots")
    ap.add_argument("--watch", default=None,
                    help="comma list of output meters to watch (default Main1,Main2)")
    ap.add_argument("--slots", default=None,
                    help="extra raw meter slots to add (comma list, e.g. 32,37,50)")
    ap.add_argument("--threshold", type=float, default=-55.0,
                    help="dBFS threshold for --motion-check (default -55)")
    ap.add_argument("--motion-check", action="store_true",
                    help="exit 3 unless a watched meter shows signal (>= -55 dBFS)")
    ap.add_argument("--print-interval", type=float, default=0.1,
                    help="console update interval in seconds (default 0.1 = 10 Hz)")
    ap.add_argument("--quiet", action="store_true", help="no per-frame console output")
    ap.add_argument("--list-indices", action="store_true", help="print the known meter slots")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args(argv)
    if args.watch:
        args.watch = [w.strip() for w in args.watch.split(",") if w.strip()]
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
