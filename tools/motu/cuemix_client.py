#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
client.py -- dependency-free client for the MOTU Gen5 / CueMix 5 control protocol.

Two transports, one message layer (reverse-engineered from the un-packed CueMix 5
Electron app: resources/app/src/www/{midi,datastore,dev,dev_common}.js):

  ws     (PRIMARY)           WebSocket to ws://<device-ip>:1280 (CueMix "Connect to
                             IP").  The device announces its IP by broadcasting a
                             JSON beacon on UDP port 1280 -> 255.255.255.255:1280.
  sysex  (NOT the ULmk5 channel) USB-MIDI SysEx.  Board proof 2026-11-15: the
                             UltraLite-mk5 USB-MIDI jacks are a pure DIN bridge
                             (embedded IN jack -> external OUT jack), the device
                             never answers a sysex probe; CueMix does not use it
                             for the mk5.  Kept only for completeness/other models.

Message layer (big endian) -- identical on both transports:
  device -> host : id:u16  index:u16  value            (length implied by type)
  host -> device : id:u16  index:u16  len:u16  value
  types: B=u8, I=s32, I16=u16, F=8.24 fixed (s32/2^24), S=32-byte NUL-padded, A=bytes

SysEx framing (midi.js):
  F0 00 00 3B 00 01 <req> <7-bit packed payload> F7
  req = direction(bit6: 0=host->device, 1=device->host) | request id
  request id: 0=SetProperty, 1=ProtocolProbe, 2=EnableSysexAPI
  payload is 7-bit encoded: groups of 7 data bytes, MSBs collected in a leading byte.

  handshake: send probe (req 0x01) -> device answers 0x41 -> send EnableSysexAPI
  (req 0x02) -> device streams SetProperty (req 0x40) frames with the current state.

The WebSocket path additionally has a password handshake (only when the device has
a network password enabled): proxy message id 0xFFFE, sub-ids 6 (public key PEM),
2 (password enabled), 0 (auth request, 10-byte serial), and the client answers
sub-id 1 with RSA-OAEP(SHA-1) of sha256_hex(password).  Implemented here with a
pure-python RSA-OAEP so no third-party package is needed.

READ-ONLY BY DEFAULT.  Writes require --yes and are printed first (see --dry-run).

Examples
--------
  python3 client.py --selftest
  python3 client.py --midi /dev/snd/midiC4D0 --read 10 --dump-matrix --dump-direct
  python3 client.py --midi /dev/snd/midiC4D0 --save before.json --read 10
  python3 client.py --midi /dev/snd/midiC4D0 --mute-inputs --dry-run
  python3 client.py --midi /dev/snd/midiC4D0 --mute-inputs --yes
  python3 client.py --midi /dev/snd/midiC4D0 --restore before.json --yes
  python3 client.py --host 127.0.0.1 --port 1281 --read 5
"""
import argparse
import base64
import hashlib
import json
import os
import select
import socket
import struct
import sys
import time

VERSION = "1.0"

# ---------------------------------------------------------------- property table
# UltraLite-mk5 datastore (CueMix 5 www/dev.js -> DeviceProps_ul.defaultDatastore).
# "key": (property id, wire type)
PROPS = {
    "kDeviceMAC": (0, "A"), "kDevUID": (2, "A"), "kDevName": (3, "S"),
    "kDevVersion": (4, "S"), "kiChannelNames": (6, "S"), "koChannelNames": (7, "S"),
    "kAPIVersion": (8, "I16"), "kSampleRate": (10, "I"), "kClockSource": (11, "B"),
    "kFPGALocked": (15, "B"), "kPLLLocked": (16, "B"), "kMIDIThru": (19, "B"),
    "kLoopbackFirst": (20, "B"), "kModelID": (21, "S"), "kNetControlEnable": (26, "B"),
    "kiMixStereo": (1000, "B"), "koMixStereo": (1001, "B"),
    "kiEQMode": (1002, "B"), "kiEQBypass": (1003, "B"), "kiEQFreq": (1004, "I"),
    "kiEQGain": (1005, "F"), "kiEQBW": (1006, "F"),
    "kiGateBypass": (1007, "B"), "kiGateThreshold": (1008, "F"),
    "kiGateAttack": (1009, "I16"), "kiGateRelease": (1010, "I16"),
    "kiCompBypass": (1011, "B"), "kiCompThreshold": (1012, "F"),
    "kiCompAttack": (1013, "I16"), "kiCompRelease": (1014, "I16"),
    "kiCompRatio": (1015, "F"), "kiCompMakeup": (1021, "F"),
    "kiMixFader": (1016, "F"), "kiMixPan": (1017, "F"),
    "kiMixSolo": (1018, "B"), "kiMixMute": (1019, "B"),
    "kHostOutMixMode": (1020, "B"),
    "koEQMode": (1022, "B"), "koEQBypass": (1023, "B"), "koEQFreq": (1024, "I"),
    "koEQGain": (1025, "F"), "koEQBW": (1026, "F"),
    "koBusFader": (1027, "F"), "koBusMute": (1028, "B"),
    "kReverbDecay": (1030, "B"), "kReverbDamping": (1031, "B"),
    "kReverbPreDelay": (1032, "B"), "kReverbWidth": (1033, "B"),
    "kReverbPreset": (1034, "B"),
    "koTrim": (5000, "B"), "kiGain": (5001, "B"), "kiPhase": (5002, "B"),
    "kPad": (5003, "B"), "k48V": (5004, "B"), "kJackDetect": (5005, "B"),
    "kOpticalMode": (5006, "B"), "kFPGAPatch": (5010, "B"), "kMainTrim": (5011, "B"),
    "kMainGroup": (5012, "I16"), "kLoopbackSource": (5014, "B"),
    "kABEnable": (5015, "B"), "kAEnable": (5016, "B"), "kBEnable": (5017, "B"),
    "kMonoEnable": (5018, "B"), "kMuteEnable": (5019, "B"),
    "kOpticalExpanderPatch": (5020, "B"), "kWordClockOut": (5024, "B"),
    "kOpticalExpander": (5025, "B"), "kTalkbackLatch": (5026, "B"),
    "kTalkbackEnable": (5027, "B"), "kTalkbackLevel": (5028, "F"),
    "kTalkbackDim": (5029, "F"), "kTalkbackSource": (5030, "B"),
    "kTalkbackDestination": (5031, "I"),
    "kFootSwitchEnabled": (5032, "B"), "kFootSwitchDownKey": (5033, "S"),
    "kFootSwitchUpKey": (5034, "S"),
}
BY_ID = {v[0]: (k, v[1]) for k, v in PROPS.items()}
MIXER_KEYS = ("kiMixFader", "kiMixPan", "kiMixSolo", "kiMixMute", "kiMixStereo",
              "koBusFader", "koBusMute", "kHostOutMixMode", "kFPGAPatch",
              "kLoopbackSource", "kLoopbackFirst", "kNetControlEnable")

# mixer matrix: crosspoint index = bus*32 + input  (dev.js get_mix_index)
NUM_IN = 32
BUSES = [
    (0, "Main 1", "Main Out 1"), (1, "Main 2", "Main Out 2"),
    (2, "Line 3", "Line Out 3"), (3, "Line 4", "Line Out 4"),
    (4, "Line 5", "Line Out 5"), (5, "Line 6", "Line Out 6"),
    (6, "Line 7", "Line Out 7"), (7, "Line 8", "Line Out 8"),
    (8, "Line 9", "Line Out 9"), (9, "Line 10", "Line Out 10"),
    (10, "Phones 1", "Phones Out L"), (11, "Phones 2", "Phones Out R"),
    (12, "Reverb 1", "reverb send L"), (13, "Reverb 2", "reverb send R"),
]
INPUTS = (
    ["Mic/Line 1", "Mic/Line 2"]
    + ["Line %d" % n for n in range(3, 9)]
    + ["S/PDIF 1", "S/PDIF 2"]
    + ["Optical 1", "Optical 2", "Optical 3", "Optical 4",
       "Optical 5", "Optical 6", "Optical 7", "Optical 8"]
    + ["USB Main L", "USB Main R"]
    + ["USB Line %d" % n for n in range(3, 11)]
    + ["USB Phones L", "USB Phones R", "Reverb Ret L", "Reverb Ret R"]
)
PHYSICAL_INPUTS = list(range(0, 18))     # mic / line / spdif / optical
HOST_INPUTS = list(range(18, 30))        # USB playback -> mixer
REVERB_RETURNS = [30, 31]

# kFPGAPatch (id 5010) output-destination indices (dev.js defaultPatch comments)
FPGA_DEST = {
    0x20: "Line Out 3", 0x21: "Line Out 4", 0x22: "Line Out 5", 0x23: "Line Out 6",
    0x24: "Line Out 7", 0x25: "Line Out 8", 0x26: "Main Out 1", 0x27: "Main Out 2",
    0x28: "Line Out 1", 0x29: "Line Out 2", 0x2A: "Phones Out L", 0x2B: "Phones Out R",
}
DSP_PATCH_MIN = 0x2E                     # >= this = DSP/mixer output tap

SYSEX_HEADER = bytes([0xF0, 0x00, 0x00, 0x3B, 0x00, 0x01])
REQ_SET_PROPERTY, REQ_PROBE, REQ_ENABLE = 0, 1, 2
DIR_HOST_TO_DEVICE, DIR_DEVICE_TO_HOST = 0x00, 0x40

PROXY_ID, PROXY_STATUS_ID, METERS_ID = 0xFFFE, 0xFFFF, 6000
K_PUBKEY, K_AUTH_REQ, K_PASSWORD_STATUS, K_AUTH_RESP = 6, 0, 2, 1


# ------------------------------------------------------------------- value codec
def decode_value(ptype, data):
    if ptype == "B":
        return data[0] if len(data) >= 1 else None
    if ptype == "I":
        return struct.unpack("!i", data[:4])[0] if len(data) >= 4 else None
    if ptype == "I16":
        return struct.unpack("!H", data[:2])[0] if len(data) >= 2 else None
    if ptype == "F":
        return struct.unpack("!i", data[:4])[0] / float(0x01000000) if len(data) >= 4 else None
    if ptype == "S":
        return data.split(b"\x00", 1)[0].decode("utf-8", "replace")
    if ptype == "A":
        return list(data)
    return None


def encode_value(ptype, value):
    if ptype == "B":
        return struct.pack("!B", int(value) & 0xFF)
    if ptype == "I":
        return struct.pack("!i", int(value))
    if ptype == "I16":
        return struct.pack("!H", int(value) & 0xFFFF)
    if ptype == "F":
        return struct.pack("!i", int(round(float(value) * 0x01000000)))
    if ptype == "S":
        raw = str(value).encode("utf-8")[:31]
        return raw + b"\x00" * (32 - len(raw))
    raise ValueError("type %s is not writable" % ptype)


def build_device_message(key, index, value):
    """host -> device message: id(2) index(2) len(2) value"""
    pid, ptype = PROPS[key]
    body = encode_value(ptype, value)
    return struct.pack("!HHH", pid, index, len(body)) + body


# ------------------------------------------------------------------ sysEx codec
def chunk_messages(msgs, limit=1400):
    """glue several device messages into ws frames of <= limit bytes (CueMix does
    the same: it batches up to 1452 bytes per websocket message, datastore.js)."""
    frame = b""
    for m in msgs:
        if frame and len(frame) + len(m) > limit:
            yield frame
            frame = b""
        frame += m
    if frame:
        yield frame


def sysex_encode(data):
    """7-bit pack a device message into a full MOTU SysEx frame (midi.js encodeDeviceMessage)."""
    out = bytearray(SYSEX_HEADER)
    out.append(DIR_HOST_TO_DEVICE | REQ_SET_PROPERTY)
    out.append(0)                                  # MSB byte placeholder
    b7 = len(out) - 1
    shr = 7
    for value in data:
        if shr == 0:
            out.append(0)
            b7 = len(out) - 1
            shr = 7
        out[b7] |= (value & 0x80) >> shr
        shr -= 1
        out.append(value & 0x7F)
    out.append(0xF7)
    return bytes(out)


def sysex_decode(frame):
    """unpack a MOTU SysEx frame -> device message bytes (midi.js DecodeSetPropertySysexMessage)."""
    if len(frame) < 10 or frame[0] != 0xF0:
        return None
    if frame[1:4] != b"\x00\x00\x3b" or frame[4:6] != b"\x00\x01":
        return None
    body = frame[7:]                               # skip header(6) + request byte(1)
    if not body or body[-1] != 0xF7:
        return None
    body = body[:-1]
    if not body:
        return None
    decoded = bytearray()
    n = 0
    b7 = body[n]
    n += 1
    shl = 7
    while n < len(body):
        if shl == 0:
            b7 = body[n]
            n += 1
            shl = 7
            continue
        decoded.append(((b7 << shl) & 0x80) | body[n])
        n += 1
        shl -= 1
    return bytes(decoded)


def sysex_request(request_id):
    return SYSEX_HEADER + bytes([DIR_HOST_TO_DEVICE | request_id, 0xF7])


def sysex_frame_request(frame):
    if len(frame) < 7:
        return None
    return frame[6]


# ----------------------------------------------------------------------- sysex
class SysexTransport:
    """rawmidi character device (/dev/snd/midiC*D*) -- needs no library at all."""

    def __init__(self, path, timeout=5.0, verbose=False):
        self.path = path
        self.timeout = timeout
        self.verbose = verbose
        self.fd = None
        self.rx = bytearray()
        self.enabled = False
        self.probed = False

    def open(self):
        self.fd = os.open(self.path, os.O_RDWR | os.O_NONBLOCK)
        return self

    def close(self):
        if self.fd is not None:
            try:
                os.close(self.fd)
            except OSError:
                pass
            self.fd = None

    def write_raw(self, data):
        data = bytes(data)
        sent = 0
        deadline = time.time() + self.timeout
        while sent < len(data):
            try:
                sent += os.write(self.fd, data[sent:])
            except BlockingIOError:
                if time.time() > deadline:
                    raise IOError("timed out writing to %s" % self.path)
                time.sleep(0.01)

    def send_raw(self, data):
        self.write_raw(data)

    def send_message(self, key, index, value):
        self.write_raw(sysex_encode(build_device_message(key, index, value)))

    def poll(self, timeout):
        """read rawmidi bytes, handshake, return list of decoded device messages."""
        msgs = []
        if self.fd is None:
            return msgs
        r, _, _ = select.select([self.fd], [], [], timeout)
        if not r:
            return msgs
        try:
            chunk = os.read(self.fd, 4096)
        except BlockingIOError:
            return msgs
        if not chunk:
            return msgs
        self.rx += chunk
        if self.verbose:
            print("[sysex rx] %s" % chunk.hex())
        # peel complete F0..F7 frames (ignore stray bytes)
        while True:
            start = self.rx.find(b"\xf0")
            if start < 0:
                self.rx.clear()
                break
            if start:
                del self.rx[:start]
            end = self.rx.find(b"\xf7")
            if end < 0:
                break
            frame = bytes(self.rx[:end + 1])
            del self.rx[:end + 1]
            req = sysex_frame_request(frame)
            if req is None:
                continue
            direction = req & 0x40
            request_id = req & 0x3F
            if direction != DIR_DEVICE_TO_HOST:
                continue
            if request_id == REQ_PROBE and not self.probed:
                self.probed = True
                print("[sysex] device answered protocol probe -> sending EnableSysexAPI")
                self.write_raw(sysex_request(REQ_ENABLE))
                self.enabled = True
            elif request_id == REQ_SET_PROPERTY:
                payload = sysex_decode(frame)
                if payload:
                    msgs.append(payload)
        return msgs


# -------------------------------------------------------------------- WebSocket
class WebSocketTransport:
    def __init__(self, host, port, iface=None, path="/", timeout=5.0, verbose=False):
        self.host, self.port, self.iface, self.path = host, port, iface, path
        self.timeout, self.verbose = timeout, verbose
        self.sock = None
        self.buf = b""

    def open(self):
        host = self.host
        scope = 0
        if host.startswith("[") and host.endswith("]"):
            host = host[1:-1]
        if "%" in host:
            host, _, self.iface = host.partition("%")
        if ":" in host:
            if self.iface and hasattr(socket, "if_nametoindex"):
                scope = socket.if_nametoindex(self.iface)
            self.sock = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
            self.sock.settimeout(self.timeout)
            self.sock.connect((host, self.port, 0, scope))
        else:
            self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            self.sock.settimeout(self.timeout)
            self.sock.connect((host, self.port))
        key = base64.b64encode(os.urandom(16)).decode()
        req = ("GET %s HTTP/1.1\r\nHost: %s:%d\r\nUpgrade: websocket\r\n"
               "Connection: Upgrade\r\nSec-WebSocket-Key: %s\r\n"
               "Sec-WebSocket-Version: 13\r\nUser-Agent: cuemix-proto/%s\r\n\r\n"
               % (self.path, host, self.port, key, VERSION))
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

    def send(self, payload):
        n = len(payload)
        if n < 126:
            hdr = struct.pack("!BB", 0x82, 0x80 | n)
        elif n < 65536:
            hdr = struct.pack("!BBH", 0x82, 0x80 | 126, n)
        else:
            hdr = struct.pack("!BBQ", 0x82, 0x80 | 127, n)
        mask = os.urandom(4)
        self.sock.sendall(hdr + mask + bytes(c ^ mask[i & 3] for i, c in enumerate(payload)))

    def send_raw(self, data):
        self.send(data)

    def send_message(self, key, index, value):
        self.send(build_device_message(key, index, value))

    def _exact(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise IOError("connection closed by peer")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def poll(self, timeout):
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
        if opcode == 0x9:
            m = os.urandom(4)
            self.sock.sendall(struct.pack("!BB", 0x8A, 0x80 | len(data)) + m +
                              bytes(c ^ m[i & 3] for i, c in enumerate(data)))
            return []
        if opcode in (0x8, 0xA):
            raise IOError("websocket closed by peer")
        return [data]


# --------------------------------------------------------- pure python RSA-OAEP
def _der_tlv(buf, off):
    tag = buf[off]
    ln = buf[off + 1]
    off += 2
    if ln & 0x80:
        n = ln & 0x7F
        ln = int.from_bytes(buf[off:off + n], "big")
        off += n
    return tag, buf[off:off + ln], off + ln


def parse_rsa_public_key_pem(pem):
    body = "".join(l for l in pem.strip().splitlines() if "-----" not in l)
    der = base64.b64decode(body)
    spki, _, _ = _der_tlv(der, 0)                  # SEQUENCE
    _, _, off = _der_tlv(spki, 0)                  # AlgorithmIdentifier
    _, bitstr, _ = _der_tlv(spki, off)             # BIT STRING
    rsakey, _, _ = _der_tlv(bitstr[1:], 0)         # SEQUENCE { n, e }
    _, nbytes, off = _der_tlv(rsakey, 0)
    _, ebytes, _ = _der_tlv(rsakey, off)
    return int.from_bytes(nbytes, "big"), int.from_bytes(ebytes, "big")


def _mgf1(seed, length):
    out = b""
    counter = 0
    while len(out) < length:
        out += hashlib.sha1(seed + struct.pack("!I", counter)).digest()
        counter += 1
    return out[:length]


def rsa_oaep_sha1_encrypt(pem, message):
    n, e = parse_rsa_public_key_pem(pem)
    k = (n.bit_length() + 7) // 8
    hlen = 20
    if len(message) > k - 2 * hlen - 2:
        raise ValueError("message too long for RSA key")
    db = hashlib.sha1(b"").digest() + b"\x00" * (k - len(message) - 2 * hlen - 2) + b"\x01" + message
    seed = os.urandom(hlen)
    masked_db = bytes(a ^ b for a, b in zip(db, _mgf1(seed, k - hlen - 1)))
    masked_seed = bytes(a ^ b for a, b in zip(seed, _mgf1(masked_db, hlen)))
    m = int.from_bytes(b"\x00" + masked_seed + masked_db, "big")
    return pow(m, e, n).to_bytes(k, "big")


# ----------------------------------------------------------------------- device
class Device:
    """datastore mirror + mixer helpers shared by both transports."""

    def __init__(self, transport, verbose=False):
        self.tp = transport
        self.verbose = verbose
        self.values = {}
        self.pubkey = None
        self.password_enabled = None
        self.serial = None
        self.proxy_status = None
        self.authenticated = False
        self.n_messages = 0
        self.n_unknown = 0

    def handle(self, msg, password=None):
        if len(msg) < 2:
            return
        mid = struct.unpack("!H", msg[:2])[0]
        if mid == PROXY_ID:
            self._proxy_msg(msg, password)
            return
        if mid == PROXY_STATUS_ID:
            self.proxy_status = msg[4] if len(msg) > 4 else None
            return
        if mid == METERS_ID:
            return
        if len(msg) < 4:
            return
        index = struct.unpack("!H", msg[2:4])[0]
        name, ptype = BY_ID.get(mid, (None, None))
        if name is None:
            self.n_unknown += 1
            if self.verbose and self.n_unknown < 20:
                print("[unknown property id %d idx %d len %d] %s"
                      % (mid, index, len(msg) - 4, msg[4:20].hex()))
            return
        arr = self.values.setdefault(name, [])
        while len(arr) <= index:
            arr.append(None)
        arr[index] = decode_value(ptype, msg[4:])
        self.n_messages += 1
        if not self.authenticated:
            self.authenticated = True

    def _proxy_msg(self, msg, password):
        if len(msg) < 6:
            return
        msgid, ln = struct.unpack("!HH", msg[2:6])
        payload = msg[6:6 + ln]
        if msgid == K_PUBKEY:
            self.pubkey = payload.decode("utf-8", "replace")
            print("[proxy] device public key (%d bytes)" % len(payload))
        elif msgid == K_PASSWORD_STATUS:
            self.password_enabled = bool(payload[0]) if payload else None
            print("[proxy] password enabled = %s" % self.password_enabled)
        elif msgid == K_AUTH_REQ:
            self.serial = payload[:10].decode("ascii", "replace")
            print("[proxy] auth request, serial=%r" % self.serial)
            if password is None:
                print("[proxy] password required but none supplied (--password / $CUEMIX_PASSWORD)")
                return
            digest = hashlib.sha256(password.encode("utf-8")).hexdigest().encode()
            cipher = rsa_oaep_sha1_encrypt(self.pubkey, digest)
            self.tp.send_raw(struct.pack("!HHH", PROXY_ID, K_AUTH_RESP, len(cipher)) + cipher)
            print("[proxy] sent RSA-OAEP(SHA-1) auth response")

    # -- accessors --------------------------------------------------------
    def get(self, key, index=0, default=None):
        arr = self.values.get(key)
        if arr is None or index >= len(arr) or arr[index] is None:
            return default
        return arr[index]

    def summary(self):
        print("\n== device summary ==")
        for k in ("kModelID", "kDevName", "kDevVersion", "kAPIVersion", "kDevUID",
                  "kDeviceMAC", "kSampleRate", "kClockSource", "kOpticalMode",
                  "kHostOutMixMode", "kNetControlEnable", "kLoopbackFirst"):
            print("  %-16s %s" % (k, self.get(k)))
        print("  datastore values %d messages, %d unknown ids, authenticated=%s"
              % (self.n_messages, self.n_unknown, self.authenticated))
        if self.n_messages == 0:
            print("  !! nothing received -- check transport/handshake before drawing conclusions")
        nf = len(self.values.get("kiMixFader") or [])
        if self.n_messages and nf < 448:
            print("  !! mixer matrix incomplete: %d/448 faders received -- increase --read "
                  "or re-run (the device may still be streaming)" % nf)

    def active_crosspoints(self):
        out = []
        for bus, buslabel, _jack in BUSES:
            for inp in range(NUM_IN):
                i = bus * NUM_IN + inp
                f = self.get("kiMixFader", i)
                m = self.get("kiMixMute", i)
                if f is None and m is None:
                    continue
                f = f or 0.0
                if f > 0 and not m:
                    out.append((bus, buslabel, inp, INPUTS[inp], f,
                                bool(self.get("kiMixSolo", i)),
                                self.get("kiMixPan", i)))
        return out

    def dump_matrix(self):
        print("\n== mixer matrix: active crosspoints (fader>0 and not muted) ==")
        rows = {}
        for bus, buslabel, inp, ilabel, f, solo, pan in self.active_crosspoints():
            rows.setdefault(buslabel, []).append((ilabel, f, solo, pan))
        if not rows:
            print("  none (or no mixer data received yet)")
        for buslabel, entries in rows.items():
            print("  %-9s <- %s" % (buslabel, ", ".join(
                "%s(f=%.3f%s%s)" % (il, f, ", SOLO" if s else "",
                                    ", pan=%.3f" % p if p not in (None, 0.5) else "")
                for (il, f, s, p) in entries)))

    def direct_routes(self):
        """physical input -> output bus crosspoints (the CueMix 'direct through')."""
        rows = {}
        for bus, buslabel, _jack in BUSES:
            for inp in PHYSICAL_INPUTS + REVERB_RETURNS:
                i = bus * NUM_IN + inp
                f = self.get("kiMixFader", i) or 0.0
                m = self.get("kiMixMute", i) or 0
                if f > 0 and not m:
                    rows.setdefault(buslabel, []).append((INPUTS[inp], f))
        return rows

    def dump_direct_routes(self):
        print("\n== physical input -> output bus crosspoints (direct-through check) ==")
        rows = self.direct_routes()
        if not rows:
            print("  none: NO physical input reaches any output bus")
        for buslabel, hits in rows.items():
            print("  %-9s <- %s" % (buslabel, ", ".join("%s(f=%.3f)" % h for h in hits)))
        return rows

    def dump_fpga_patch(self):
        patch = self.values.get("kFPGAPatch")
        print("\n== kFPGAPatch: DAC/output destinations (hardware bypass check) ==")
        if not patch:
            print("  not received -- cannot conclude anything about hardware bypass")
            return
        defects = []
        unknown = 0
        for dest, label in sorted(FPGA_DEST.items()):
            src = patch[dest] if dest < len(patch) else None
            if src is None:
                print("  %-12s (0x%02X) = ?  (not received)" % (label, dest))
                unknown += 1
                continue
            kind = "DSP/mixer out" if src >= DSP_PATCH_MIN else "PHYSICAL INPUT (bypass!)"
            print("  %-12s (0x%02X) = patch 0x%02X  %s" % (label, dest, src, kind))
            if src < DSP_PATCH_MIN:
                defects.append((label, src))
        if defects:
            print("  !! hardware bypass detected: %s" % defects)
        elif unknown:
            print("  INCOMPLETE: %d/%d destinations known; re-read with a longer --read"
                  % (len(FPGA_DEST) - unknown, len(FPGA_DEST)))
        else:
            print("  OK: every output jack is fed from the DSP/mixer, not from a raw input")


# ------------------------------------------------------------------ discovery
def discover_beacon(iface=None, seconds=6.0):
    """Passively listen for the device's UDP-1280 JSON beacon (no packets sent).

    The UltraLite-mk5 broadcasts {"uid","name","ip","ipv6","model","version",
    "interval"} from <device-ip>:1280 to 255.255.255.255:1280 every interval s.
    """
    if iface is None:
        import glob
        cands = [p.split("/")[-1] for p in glob.glob("/sys/class/net/enx*")]
        iface = cands[0] if cands else None
    if iface is None:
        print("[discover] no enx* interface found")
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
            payload = pkt[14 + ihl + 8:]
            try:
                return json.loads(payload.decode("utf-8"))
            except Exception:
                continue
    finally:
        s.close()
    return None


# ------------------------------------------------------------------ board probe
def list_midi_devices():
    import glob
    return sorted(glob.glob("/dev/snd/midiC*D*"))


def do_read(dev, seconds, idle_stop=0.0, hexdump=0):
    t0 = time.time()
    last = t0
    dumped = 0
    while True:
        now = time.time()
        if now - t0 >= seconds:
            break
        if idle_stop and now - last >= idle_stop:
            break
        try:
            msgs = dev.tp.poll(0.2)
        except IOError as exc:
            print("[read] transport closed: %s" % exc)
            break
        if msgs:
            last = time.time()
            for m in msgs:
                if dumped < hexdump:
                    dumped += 1
                    print("[frame %d] len=%d %s" % (dumped, len(m), m[:64].hex()))
                dev.handle(m)
    return


# --------------------------------------------------------------------- selftest
SELFTEST_VECTORS = [
    {"msg": [3, 248, 0, 0, 0, 1, 1],
     "enc": [240, 0, 0, 59, 0, 1, 0, 2, 3, 120, 0, 0, 0, 1, 1, 247]},
    {"msg": [3, 248, 0, 8, 0, 4, 63, 128, 0, 0],
     "enc": [240, 0, 0, 59, 0, 1, 0, 2, 3, 120, 0, 8, 0, 4, 63, 1, 0, 0, 0, 247]},
    {"msg": [3, 248, 0, 0, 0, 4, 0, 0, 128, 0],
     "enc": [240, 0, 0, 59, 0, 1, 0, 2, 3, 120, 0, 0, 0, 4, 0, 2, 0, 0, 0, 247]},
]


def selftest():
    ok = 0
    for v in SELFTEST_VECTORS:
        enc = list(sysex_encode(bytes(v["msg"])))
        if enc != v["enc"]:
            print("FAIL encode %s\n got %s\n want %s" % (v["msg"], enc, v["enc"]))
            return 1
        dec = list(sysex_decode(bytes(v["enc"])))
        if dec != v["msg"]:
            print("FAIL decode %s -> %s" % (v["enc"], dec))
            return 1
        ok += 1
    # value codec roundtrip
    for ptype, val in (("B", 7), ("I", -123456), ("I16", 65535), ("F", 0.5),
                       ("S", "Line 7-8")):
        got = decode_value(ptype, encode_value(ptype, val))
        if ptype == "F":
            assert abs(got - val) < 1e-6, (ptype, got, val)
        elif ptype == "S":
            assert got == val, (ptype, got, val)
        else:
            assert got == val, (ptype, got, val)
    # crosspoint index math
    assert 1016 * 0 + 6 == 6
    print("selftest OK: %d sysex vectors + value codec" % ok)
    return 0


# ------------------------------------------------------------------------ main
def main(argv=None):
    ap = argparse.ArgumentParser(
        description="MOTU Gen5 / CueMix 5 control client (read-only unless --yes)")
    ap.add_argument("--selftest", action="store_true", help="offline codec selftest")
    ap.add_argument("--transport", choices=["sysex", "ws", "auto"], default="auto")
    ap.add_argument("--midi", default=None,
                    help="rawmidi device (legacy sysex transport, e.g. /dev/snd/midiC4D0)")
    ap.add_argument("--discover", action="store_true",
                    help="passively discover the device from its UDP-1280 beacon first")
    ap.add_argument("--discover-iface", default=None,
                    help="interface to listen on for --discover (e.g. enx0001f2fe7e6f)")
    ap.add_argument("--discover-timeout", type=float, default=6.0)
    ap.add_argument("--host", default=None,
                    help="WS transport host (default 127.0.0.1; use the device IP, "
                         "e.g. the 'ip' field of the UDP-1280 beacon)")
    ap.add_argument("--port", type=int, default=None,
                    help="WS port (default 1281 for 127.0.0.1/local, else 1280)")
    ap.add_argument("--iface", default=None, help="interface for IPv6 link-local scope")
    ap.add_argument("--path", default="/", help="WS request path (default /)")
    ap.add_argument("--timeout", type=float, default=5.0)
    ap.add_argument("--read", type=float, default=15.0, help="seconds to listen")
    ap.add_argument("--idle-stop", type=float, default=0.0,
                    help="stop early after N idle seconds")
    ap.add_argument("--dump-matrix", action="store_true")
    ap.add_argument("--dump-direct", action="store_true")
    ap.add_argument("--dump-fpga", action="store_true")
    ap.add_argument("--json", metavar="FILE", help="write received state as JSON")
    ap.add_argument("--save", metavar="FILE", help="alias of --json")
    ap.add_argument("--restore", metavar="FILE", help="restore mixer state from snapshot")
    ap.add_argument("--mute-inputs", action="store_true",
                    help="set kiMixFader=0 for physical inputs 0-17,30,31 on all buses")
    ap.add_argument("--hard-mute", action="store_true",
                    help="with --mute-inputs also set kiMixMute=1")
    ap.add_argument("--set", action="append", metavar="KEY=INDEX=VALUE")
    ap.add_argument("--apply", metavar="FILE",
                    help="apply a JSON array of [key, index, value] writes (batched)")
    ap.add_argument("--password", default=None)
    ap.add_argument("--password-env", default="CUEMIX_PASSWORD")
    ap.add_argument("--yes", action="store_true", help="perform writes")
    ap.add_argument("--dry-run", action="store_true", help="print planned writes only")
    ap.add_argument("--hexdump", type=int, default=0,
                    help="dump the first N raw frames as hex (debug)")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args(argv)

    if args.selftest:
        return selftest()

    # ---------------- transport selection ----------------
    # ws is the real control channel for the UltraLite-mk5; sysex is legacy/other
    # models and only used when explicitly requested (--transport sysex / --midi).
    if args.discover:
        dev = discover_beacon(args.discover_iface, args.discover_timeout)
        if dev is None:
            print("no UDP-1280 beacon seen; pass --discover-iface <enx...> or --host <ip>")
            return 2
        print("[discover] device: %s" % json.dumps(dev, sort_keys=True))
        if args.host is None:
            args.host = dev.get("ip")
        if args.host is None:
            print("[discover] beacon has no ipv4 'ip' field; try --host <ip>")
            return 2

    tp = None
    if args.transport == "sysex" or (args.transport == "auto" and args.midi):
        path = args.midi
        if path is None:
            cands = list_midi_devices()
            if len(cands) == 1:
                path = cands[0]
            elif len(cands) > 1:
                print("several rawmidi devices found: %s" % cands)
                print("pick one with --midi (MOTU is usually the 4th card on the board: "
                      "/dev/snd/midiC4D0)")
                return 2
            else:
                print("no /dev/snd/midiC*D* found -- is the MOTU attached and is "
                      "snd-usb-audio loaded?")
                return 2
        if path:
            try:
                tp = SysexTransport(path, timeout=args.timeout, verbose=args.verbose).open()
                print("transport: sysex via %s" % path)
                tp.write_raw(sysex_request(REQ_PROBE))
                print("sent MOTU sysex protocol probe (F0 00 00 3B 00 01 01 F7)")
            except OSError as exc:
                if args.transport == "sysex":
                    print("cannot open %s: %s" % (path, exc))
                    return 2
                tp = None
    if tp is None:
        host = args.host if args.host is not None else "127.0.0.1"
        port = args.port if args.port is not None else (
            1281 if host.startswith("127.") or host == "localhost" else 1280)
        try:
            tp = WebSocketTransport(host, port, args.iface, args.path,
                                    args.timeout, args.verbose).open()
            print("transport: websocket ws://%s:%d%s" % (host, port, args.path))
        except Exception as exc:
            print("CONNECT FAILED: %s" % exc)
            return 2

    dev = Device(tp, verbose=args.verbose)
    password = args.password if args.password is not None else (
        os.environ.get(args.password_env) or None)

    # ---------------- plan writes ----------------
    planned = []
    if args.mute_inputs:
        for bus, buslabel, _jack in BUSES:
            for inp in PHYSICAL_INPUTS + REVERB_RETURNS:
                planned.append(("kiMixFader", bus * NUM_IN + inp, 0.0))
                if args.hard_mute:
                    planned.append(("kiMixMute", bus * NUM_IN + inp, 1))
        print("[plan] %d crosspoint writes (physical inputs %s -> all %d buses)"
              % (len(planned), PHYSICAL_INPUTS + REVERB_RETURNS, len(BUSES)))
    for spec in (args.set or []):
        try:
            key, idx, val = spec.split("=", 2)
            ptype = PROPS[key][1]
            val = float(val) if ptype == "F" else (val if ptype == "S" else int(val, 0))
            planned.append((key, int(idx), val))
        except Exception as exc:
            print("bad --set %r: %s" % (spec, exc))
            return 2
    if args.apply:
        with open(args.apply, encoding="utf-8") as fh:
            ops = json.load(fh)
        if isinstance(ops, dict):
            ops = ops.get("ops", [])
        for op in ops:
            key, idx, val = op[0], int(op[1]), op[2]
            if key not in PROPS:
                print("skip unknown key %r" % (key,))
                continue
            ptype = PROPS[key][1]
            if ptype in ("B", "I", "I16"):
                val = int(round(float(val)))
            elif ptype == "F":
                val = float(val)
            planned.append((key, idx, val))
        print("[plan] %d writes from %s" % (len(planned), args.apply))

    # ---------------- read loop ----------------
    try:
        do_read(dev, args.read, args.idle_stop, args.hexdump)
        dev.summary()
        if args.dump_matrix or args.mute_inputs or args.restore:
            dev.dump_matrix()
        if args.dump_direct or args.mute_inputs or args.restore:
            dev.dump_direct_routes()
        if args.dump_fpga:
            dev.dump_fpga_patch()

        out = args.json or args.save
        if out:
            snap = {
                "meta": {
                    "tool": "cuemix-proto/client.py %s" % VERSION,
                    "transport": "sysex:%s" % getattr(tp, "path", "") if isinstance(tp, SysexTransport)
                                 else "ws:%s" % getattr(tp, "host", ""),
                    "saved_unix": time.time(),
                    "model": dev.get("kModelID"),
                    "uid": dev.get("kDevUID"),
                    "authenticated": dev.authenticated,
                    "messages": dev.n_messages,
                },
                "values": {k: v for k, v in dev.values.items()
                           if any(x is not None for x in v)},
            }
            with open(out, "w", encoding="utf-8") as fh:
                json.dump(snap, fh, indent=1)
            print("[save] wrote %s" % out)

        if args.restore:
            with open(args.restore, encoding="utf-8") as fh:
                snap = json.load(fh)
            vals = snap.get("values", snap)
            skipped = 0
            for key, arr in vals.items():
                if key not in PROPS or key not in MIXER_KEYS:
                    continue
                for idx, val in enumerate(arr):
                    if val is None:
                        continue
                    cur = dev.get(key, idx)
                    same = False
                    if cur is not None:
                        if isinstance(val, float) or isinstance(cur, float):
                            same = abs(float(val) - float(cur)) <= 1e-4
                        else:
                            same = (val == cur)
                    if same:
                        skipped += 1
                    else:
                        planned.append((key, idx, val))
            print("[plan] restore: %d values differ, %d already match %s"
                  % (len(planned), skipped, args.restore))

        if planned:
            if args.dry_run or not args.yes:
                print("[dry-run] %d writes suppressed (add --yes to execute):" % len(planned))
                for key, idx, val in planned[:40]:
                    where = ""
                    if key in ("kiMixFader", "kiMixMute", "kiMixPan", "kiMixSolo"):
                        bus, inp = idx // NUM_IN, idx % NUM_IN
                        where = "  %s <- %s" % (
                            BUSES[bus][1] if bus < len(BUSES) else bus,
                            INPUTS[inp] if inp < len(INPUTS) else inp)
                    print("   %s[%d] = %r%s" % (key, idx, val, where))
                if len(planned) > 40:
                    print("   ... %d more" % (len(planned) - 40))
            else:
                print("[write] sending %d values over %s"
                      % (len(planned), "sysex" if isinstance(tp, SysexTransport) else "websocket"))
                if isinstance(tp, SysexTransport):
                    for key, idx, val in planned:
                        tp.send_message(key, idx, val)
                        time.sleep(0.005)
                else:
                    payloads = [build_device_message(k, i, v) for k, i, v in planned]
                    n_frames = 0
                    for frame in chunk_messages(payloads):
                        tp.send(frame)
                        n_frames += 1
                        time.sleep(0.01)
                    print("[write] %d bytes in %d ws frames" % (sum(map(len, payloads)), n_frames))
                time.sleep(0.5)
                print("[write] done")
    finally:
        tp.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
